"""Build an isolated, hash-bound Windows portable directory and ZIP.

Run after release build, with an approved source inventory and offline cargo metadata.
This script does not build, launch, sign, publish, or install anything. It inspects PE
imports and bundles compiler runtime DLLs only from the explicitly approved directory.
The package must still receive actual relocated/native-window verification.
Python inventories support the cp311 legacy ZIP layout and cp314 installed Lib layout.
"""
from pathlib import Path, PurePosixPath
import argparse
import datetime
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tomllib
import zipfile


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def require_file(path, label):
    require(path.is_file() and not path.is_symlink(), f"Input path missing: {label}: {path}")


def safe_relative(root, relative):
    pure = PurePosixPath(relative)
    require(not pure.is_absolute() and '..' not in pure.parts and '\\' not in relative, 'Unsafe inventory path')
    path = root / relative
    require(path.resolve().is_relative_to(root.resolve()) and path.is_file() and not path.is_symlink(), 'Missing or linked source')
    return path


INVENTORY_IGNORED_DIRS = frozenset({'.git', '.claude', '__pycache__', 'dist', 'node_modules', 'target'})


def source_files(root):
    return {
        path.relative_to(root).as_posix()
        for path in root.rglob('*')
        if path.is_file() and not path.is_symlink()
        and not any(part in INVENTORY_IGNORED_DIRS for part in path.relative_to(root).parts)
    }


def verify_sources(root, files):
    actual = source_files(root)
    expected = set(files)
    if actual != expected:
        missing = sorted(expected - actual)
        unlisted = sorted(actual - expected)
        details = []
        if missing:
            details.append('missing=' + ','.join(missing))
        if unlisted:
            details.append('unlisted=' + ','.join(unlisted))
        require(False, 'Source inventory closure mismatch: ' + '; '.join(details))
    for relative, spec in files.items():
        path = safe_relative(root, relative)
        require(digest(path) == spec['sha256'] and path.stat().st_size == spec['bytes'], 'Source inventory drift: ' + relative)


RUNTIME_SCHEMA = 'ao-windows-python-runtime/1'
EXPECTED_RUNTIME_PLATFORM = 'windows-x86_64'
RUNTIME_CONTRACTS = {
    'cp311-win_amd64': {
        'python_prefix': '3.11.',
        'interpreter_dll': 'python311.dll',
        'required_files': ('python.exe', 'python311.dll', 'python311.zip', 'Lib/site-packages/paramiko/__init__.py'),
    },
    'cp314-win_amd64': {
        'python_prefix': '3.14.',
        'interpreter_dll': 'python314.dll',
        'required_files': ('python.exe', 'python314.dll', 'Lib/site-packages/paramiko/__init__.py'),
    },
}


FORBIDDEN_RUNTIME_NAMES = frozenset({
    'known_hosts', 'id_rsa', 'id_ed25519', 'id_ecdsa', 'private_key',
    'authorized_keys', 'credentials', 'secrets',
})


def runtime_file(runtime_dir, relative):
    pure = PurePosixPath(relative)
    require(
        isinstance(relative, str)
        and relative == pure.as_posix()
        and not pure.is_absolute()
        and '..' not in pure.parts
        and ':' not in relative
        and '\\' not in relative
        and all(not part.endswith(('.', ' ')) for part in pure.parts),
        'Unsafe runtime inventory path',
    )
    require(not any(part.lower() in FORBIDDEN_RUNTIME_NAMES for part in pure.parts), 'Forbidden runtime credential path: ' + relative)
    path = runtime_dir / relative
    require(
        path.resolve().is_relative_to(runtime_dir.resolve())
        and path.is_file()
        and not path.is_symlink(),
        'Bundled Python runtime file missing: ' + relative,
    )
    return path


def require_x64_pe(path, label):
    raw = path.read_bytes()
    require(len(raw) >= 90 and raw[:2] == b'MZ', 'Python runtime PE header invalid: ' + label)
    pe_offset = int.from_bytes(raw[60:64], 'little')
    require(pe_offset + 26 <= len(raw) and raw[pe_offset:pe_offset + 4] == b'PE\0\0', 'Python runtime PE header invalid: ' + label)
    require(int.from_bytes(raw[pe_offset + 4:pe_offset + 6], 'little') == 0x8664, 'runtime PE architecture mismatch: ' + label)
    require(int.from_bytes(raw[pe_offset + 24:pe_offset + 26], 'little') == 0x20b, 'runtime PE optional header mismatch: ' + label)


def load_python_runtime(runtime_dir, inventory_path):
    require_file(inventory_path, 'Python runtime inventory')
    require(runtime_dir.is_dir() and not runtime_dir.is_symlink(), 'Input path missing: Python runtime directory: ' + str(runtime_dir))
    inventory = json.loads(inventory_path.read_text(encoding='utf-8'))
    require(inventory.get('schema') == RUNTIME_SCHEMA, 'Unsupported Python runtime inventory schema')
    require(inventory.get('platform') == EXPECTED_RUNTIME_PLATFORM, 'Python runtime platform mismatch')
    abi = inventory.get('abi')
    contract = RUNTIME_CONTRACTS.get(abi)
    require(contract is not None, 'Python runtime ABI mismatch')
    python_version = inventory.get('python_version')
    paramiko_version = inventory.get('paramiko_version')
    require(isinstance(python_version, str) and python_version, 'Python runtime version is required')
    require(python_version.startswith(contract['python_prefix']), 'runtime ABI mismatch: Python version does not match ' + abi)
    require(isinstance(paramiko_version, str) and paramiko_version, 'Paramiko runtime version is required')
    files = inventory.get('files')
    require(isinstance(files, dict) and files, 'Python runtime file inventory is required')
    for relative, spec in files.items():
        path = runtime_file(runtime_dir, relative)
        require(isinstance(spec, dict), 'Python runtime file spec is invalid: ' + relative)
        expected_sha = spec.get('sha256')
        expected_bytes = spec.get('bytes')
        require(isinstance(expected_sha, str) and re.fullmatch(r'[0-9a-f]{64}', expected_sha), 'Python runtime hash is invalid: ' + relative)
        require(isinstance(expected_bytes, int) and expected_bytes >= 0, 'Python runtime byte count is invalid: ' + relative)
        require(isinstance(spec.get('version'), str) and spec['version'], 'Python runtime file version is required: ' + relative)
        require(isinstance(spec.get('license'), str) and spec['license'], 'Python runtime file license is required: ' + relative)
        require(digest(path) == expected_sha and path.stat().st_size == expected_bytes, 'Python runtime inventory drift: ' + relative)
        if relative in ('python.exe', contract['interpreter_dll']):
            require_x64_pe(path, relative)
    for required in contract['required_files']:
        require(required in files, 'Python runtime inventory missing required file: ' + required)
    actual = {
        path.relative_to(runtime_dir).as_posix()
        for path in runtime_dir.rglob('*')
        if path.is_file() and not path.is_symlink()
    }
    require(actual == set(files), 'Python runtime inventory closure mismatch')
    helper = inventory.get('helper')
    require(isinstance(helper, dict) and helper.get('relative') == 'tools/paramiko_gateway.py', 'Python runtime helper contract is invalid')
    require(isinstance(helper.get('sha256'), str) and re.fullmatch(r'[0-9a-f]{64}', helper['sha256']), 'Python runtime helper hash is invalid')
    require(isinstance(helper.get('bytes'), int) and helper['bytes'] >= 0, 'Python runtime helper byte count is invalid')
    require(isinstance(helper.get('version'), str) and helper['version'], 'Python runtime helper version is required')
    require(isinstance(helper.get('license'), str) and helper['license'], 'Python runtime helper license is required')
    return {
        'schema': RUNTIME_SCHEMA,
        'platform': inventory['platform'],
        'abi': inventory['abi'],
        'python_version': python_version,
        'paramiko_version': paramiko_version,
        'files': {relative: dict(spec) for relative, spec in sorted(files.items())},
        'helper': dict(helper),
    }


def cargo_context(root):
    manifest = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
    toolchain = tomllib.loads((root / 'rust-toolchain.toml').read_text(encoding='utf-8'))
    return {
        'package_name': manifest['package']['name'],
        'package_version': manifest['package']['version'],
        'toolchain': toolchain['toolchain']['channel'],
        'cargo_lock_sha256': digest(root / 'Cargo.lock'),
        'cargo_toml_sha256': digest(root / 'Cargo.toml'),
        'rust_toolchain_sha256': digest(root / 'rust-toolchain.toml'),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-root', type=Path, required=True)
    parser.add_argument('--inventory', type=Path, required=True)
    parser.add_argument('--metadata', type=Path, required=True)
    parser.add_argument('--build-identity', type=Path, required=True)
    parser.add_argument('--binary-dir', '--release-dir', dest='binary_dir', type=Path, required=True)
    parser.add_argument('--profile', choices=('debug', 'release'), required=True)
    parser.add_argument('--output-parent', type=Path, required=True)
    parser.add_argument('--runtime-dir', type=Path, default=Path('C:/msys64/mingw64/bin'))
    parser.add_argument('--python-runtime-dir', type=Path)
    parser.add_argument('--python-runtime-inventory', type=Path)
    parser.add_argument('--objdump', type=Path, default=Path('C:/msys64/mingw64/bin/objdump.exe'))
    parser.add_argument('--system-dir', type=Path, default=Path('C:/Windows/System32'))
    args = parser.parse_args()
    root = args.source_root.resolve()
    output_parent = args.output_parent.resolve()
    require('dist' in output_parent.parts, 'Package output must be inside dist')
    output_parent.mkdir(parents=True, exist_ok=True)
    require_file(args.inventory, 'source inventory')
    require_file(args.metadata, 'Cargo metadata')
    require_file(args.build_identity, 'build identity')
    require(args.source_root.is_dir(), f'Input path missing: source root: {args.source_root}')
    require(args.binary_dir.is_dir(), f'Input path missing: binary directory: {args.binary_dir}')
    require(args.runtime_dir.is_dir(), f'Input path missing: runtime directory: {args.runtime_dir}')
    require(args.system_dir.is_dir(), f'Input path missing: system directory: {args.system_dir}')
    require((args.python_runtime_dir is None) == (args.python_runtime_inventory is None), 'Python runtime directory and inventory must be provided together')
    python_runtime = None
    runtime_inventory_sha = None
    if args.python_runtime_dir is not None:
        runtime_inventory_sha = digest(args.python_runtime_inventory)
        python_runtime = load_python_runtime(args.python_runtime_dir, args.python_runtime_inventory)
        python_runtime['inventory_sha256'] = runtime_inventory_sha
        python_runtime['interpreter'] = 'runtime/python/python.exe'
        python_runtime['runtime_verification'] = 'NOT_RUN_BY_PACKAGER'
        python_runtime['rust_transport_wiring'] = 'NOT_IMPLEMENTED_BY_PACKAGER'
    source = json.loads(args.inventory.read_text(encoding='utf-8'))
    files = source['files']
    require(files and all(relative in files for relative in ('Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'packaging/protocol-v1.md', 'README.md')), 'Incomplete source inventory')
    helper_in_inventory = 'tools/paramiko_gateway.py' in files
    verify_sources(root, files)
    if python_runtime is not None:
        require(helper_in_inventory, 'Bundled Python runtime requires inventory-bound Paramiko helper')
        helper_path = safe_relative(root, 'tools/paramiko_gateway.py')
        helper_spec = python_runtime['helper']
        require(digest(helper_path) == helper_spec['sha256'] and helper_path.stat().st_size == helper_spec['bytes'], 'Python runtime helper identity mismatch')
    canonical = json.dumps(files, sort_keys=True, separators=(',', ':')).encode('utf-8')
    source_digest = hashlib.sha256(canonical).hexdigest()
    context = cargo_context(root)
    identity = json.loads(args.build_identity.read_text(encoding='utf-8'))
    require(identity.get('source_sha256') == source_digest, 'Build identity source mismatch')
    require(identity.get('target') == 'x86_64-pc-windows-gnu', 'Build identity target mismatch')
    require(identity.get('metadata_sha256') == digest(args.metadata), 'Build identity metadata mismatch')
    require(identity.get('toolchain') == context['toolchain'] == '1.99.1', 'Build identity toolchain mismatch')
    require(identity.get('package_version') == context['package_version'] == '0.1.0', 'Build identity package version mismatch')
    require(files['Cargo.lock']['sha256'] == context['cargo_lock_sha256'], 'Cargo.lock context mismatch')
    require(files['Cargo.toml']['sha256'] == context['cargo_toml_sha256'], 'Cargo.toml context mismatch')
    require(files['rust-toolchain.toml']['sha256'] == context['rust_toolchain_sha256'], 'rust-toolchain context mismatch')
    require(identity.get('profile') == args.profile, 'Build identity profile mismatch')
    expected_executables = ('ao-reason-workbench.exe', 'aw-cli.exe')
    identity_executables = identity.get('executables') or {}
    require(set(identity_executables) == set(expected_executables), 'Build identity executable set mismatch')
    for binary in expected_executables:
        binary_path = args.binary_dir / binary
        require(binary_path.is_file() and not binary_path.is_symlink() and binary_path.read_bytes()[:2] == b'MZ', 'Release PE missing: ' + binary)
        spec = identity_executables[binary]
        require(spec.get('sha256') == digest(binary_path) and spec.get('bytes') == binary_path.stat().st_size, f'Executable identity drift: {binary}')
    name = 'ao-reason-workbench-0.1.0-windows-x86_64-' + args.profile + '-' + source_digest[:16]
    output = output_parent / name
    archive = output_parent / (name + '.zip')
    existing_names = {path.name for path in output_parent.iterdir()} if output_parent.is_dir() else set()
    require(name not in existing_names and (name + '.zip') not in existing_names, 'Package destination already exists; retain prior evidence')
    metadata = json.loads(args.metadata.read_text(encoding='utf-8'))
    require(metadata['workspace_root'].replace('\\', '/').lower() == str(root).replace('\\', '/').lower(), 'Metadata source root mismatch')
    resolved_ids = {node['id'] for node in metadata['resolve']['nodes']}
    package_ids = {package['id'] for package in metadata['packages']}
    missing_package_ids = sorted(resolved_ids - package_ids)
    require(not missing_package_ids, 'Cargo metadata resolved node missing package record: ' + ','.join(missing_package_ids))
    packages = [package for package in metadata['packages'] if package['id'] in resolved_ids]
    locked = tomllib.loads((root / 'Cargo.lock').read_text(encoding='utf-8')).get('package', [])
    locked_ids = {(package['name'], package['version'], package.get('source')) for package in locked}
    for package in packages:
        key = (package['name'], package['version'], package.get('source'))
        require(key in locked_ids, 'Cargo metadata package mismatch: ' + package['id'])
    require(all(package.get('license') or package.get('license_file') for package in packages), 'Dependency license metadata absent')
    for binary in ('ao-reason-workbench.exe', 'aw-cli.exe'):
        path = args.binary_dir / binary
        require(path.is_file() and path.read_bytes()[:2] == b'MZ', 'Release PE missing: ' + binary)
    output.mkdir(parents=True, exist_ok=False)
    record = {'status': 'PACKAGING', 'created_utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
        'source_sha256': source_digest, 'source_inventory_sha256': digest(args.inventory),
        'cargo_lock_sha256': context['cargo_lock_sha256'], 'metadata_sha256': digest(args.metadata),
        'cargo_toml_sha256': context['cargo_toml_sha256'], 'rust_toolchain_sha256': context['rust_toolchain_sha256'],
        'build_identity_sha256': digest(args.build_identity),
        'target': 'x86_64-pc-windows-gnu', 'toolchain': context['toolchain'], 'package_version': context['package_version'], 'profile': args.profile,
        'signing': 'UNSIGNED', 'native_runtime_verification': 'NOT_RUN', 'linux_native_verification': 'NOT_RUN_BY_PACKAGER',
        'live_ao_reason_model_verification': 'NOT_AUTHORIZED_NOT_RUN', 'executables': {
            binary: {'sha256': digest(args.binary_dir / binary), 'bytes': (args.binary_dir / binary).stat().st_size}
            for binary in ('ao-reason-workbench.exe', 'aw-cli.exe')},
        'imports': {}, 'runtime_dlls': [], 'dependencies': [], 'python_runtime': python_runtime}
    try:
        for binary in ('ao-reason-workbench.exe', 'aw-cli.exe'):
            shutil.copyfile(args.binary_dir / binary, output / binary)
        queue = [output / binary for binary in ('ao-reason-workbench.exe', 'aw-cli.exe')]
        inspected = set()
        while queue:
            binary = queue.pop(0)
            if binary.name.lower() in inspected:
                continue
            inspected.add(binary.name.lower())
            try:
                result = subprocess.run([str(args.objdump), '-p', str(binary)], capture_output=True, timeout=30)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError('PE import inspection timed out: ' + binary.name) from error
            require(result.returncode == 0, 'PE import inspection failed: ' + binary.name)
            imports = sorted(set(re.findall(r'DLL Name:\s+([^\s]+)', result.stdout.decode('utf-8', errors='replace'))), key=str.lower)
            require(imports, 'No PE imports observed: ' + binary.name)
            rows = []
            for dll in imports:
                require(Path(dll).name == dll and '/' not in dll and '\\' not in dll, 'Unexpected PE import path')
                lowered = dll.lower()
                if lowered.startswith(('api-ms-win-', 'ext-ms-win-')):
                    rows.append({'name': dll, 'classification': 'windows-api-set'})
                elif (args.system_dir / dll).is_file():
                    rows.append({'name': dll, 'classification': 'windows-system'})
                else:
                    runtime_dll = args.runtime_dir / dll
                    if runtime_dll.is_symlink() or not runtime_dll.resolve().is_relative_to(args.runtime_dir.resolve()):
                        raise RuntimeError('Runtime DLL is outside approved runtime directory: ' + dll)
                    if runtime_dll.is_file():
                        destination = output / dll
                        if not destination.exists():
                            shutil.copyfile(runtime_dll, destination)
                            queue.append(destination)
                            record['runtime_dlls'].append({'name': dll, 'source': str(runtime_dll), 'sha256': digest(destination)})
                        rows.append({'name': dll, 'classification': 'bundled-toolchain-runtime'})
                    else:
                        raise RuntimeError('Unresolved PE dependency: ' + dll)
            record['imports'][binary.name] = rows
        for package in sorted(packages, key=lambda item: item['id']):
            if not package.get('source'):
                continue
            manifest = Path(package['manifest_path'])
            crate = manifest.parent
            package_key = hashlib.sha256(package['id'].encode('utf-8')).hexdigest()[:12]
            license_root = output / 'third-party-licenses' / package_key
            license_paths = set()
            for path in crate.rglob('*'):
                if path.is_file() and not path.is_symlink() and (
                    path.name.lower().startswith(('license', 'licence', 'copying', 'notice', 'authors'))
                    or path.suffix.lower() in ('.license', '.txt')
                ):
                    license_paths.add(path)
            if package.get('license_file'):
                explicit = Path(package['license_file'])
                if not explicit.is_absolute():
                    explicit = crate / explicit
                require(explicit.is_file() and explicit.resolve().is_relative_to(crate.resolve()), 'Dependency license_file missing or escaped crate')
                license_paths.add(explicit)
            entry = {'name': package['name'], 'version': package['version'], 'license': package.get('license'), 'source': package['source'], 'license_files': []}
            for path in sorted(license_paths):
                relative = path.relative_to(crate)
                destination = license_root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, destination)
                entry['license_files'].append({'file': destination.relative_to(output).as_posix(), 'sha256': digest(destination)})
            entry['license_text_status'] = 'INCLUDED' if entry['license_files'] else 'CRATE_LICENSE_METADATA_ONLY_REVIEW_REQUIRED'
            record['dependencies'].append(entry)
        # Preserve the installed toolchain's runtime licensing texts when a runtime DLL is bundled.
        if record['runtime_dlls']:
            share = args.runtime_dir.parent / 'share/licenses'
            for directory in ('gcc-libs', 'gcc', 'winpthreads', 'libwinpthread', 'mingw-w64', 'mingw-w64-crt', 'mingw-w64-headers', 'msys2-runtime'):
                folder = share / directory
                if folder.is_dir():
                    for path in folder.rglob('*'):
                        if path.is_file() and not path.is_symlink():
                            destination = output / 'third-party-licenses/toolchain' / directory / path.relative_to(folder)
                            destination.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copyfile(path, destination)
        if helper_in_inventory:
            helper_destination = output / 'tools' / 'paramiko_gateway.py'
            helper_destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(safe_relative(root, 'tools/paramiko_gateway.py'), helper_destination)
        if python_runtime is not None:
            for relative, spec in python_runtime['files'].items():
                source_path = runtime_file(args.python_runtime_dir, relative)
                destination = output / 'runtime' / 'python' / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source_path, destination)
                require(digest(destination) == spec['sha256'] and destination.stat().st_size == spec['bytes'], 'Packaged Python runtime identity drift: ' + relative)
        shutil.copyfile(args.inventory, output / 'SOURCE-INVENTORY.json')
        if python_runtime is not None:
            shutil.copyfile(args.python_runtime_inventory, output / 'PYTHON-RUNTIME-INVENTORY.json')
        shutil.copyfile(args.metadata, output / 'CARGO-METADATA.json')
        require(digest(output / 'CARGO-METADATA.json') == identity['metadata_sha256'] == record['metadata_sha256'], 'Packaged metadata identity drift')
        shutil.copyfile(args.build_identity, output / 'BUILD-IDENTITY.json')
        shutil.copyfile(safe_relative(root, 'README.md'), output / 'README.md')
        for relative in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'packaging/protocol-v1.md', 'README.md'):
            path = safe_relative(root, relative)
            destination = output / 'source-identity' / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, destination)
        (output / 'PACKAGE-NOTES.txt').write_text(
            'AO + Reason Workbench 0.1.0 - Windows x86_64 GNU\n'
            'Double-click ao-reason-workbench.exe. Keep aw-cli.exe beside it.\n'
            'Local experience is a Rust controlled fixture, NOT real AO/Reason/model execution.\n'
            + ('Bundled Python runtime is included at runtime/python/ with hash-bound ABI/version/license inventory; use runtime/python/python.exe for the explicitly configured Paramiko gateway.\n'
               + f"Python runtime version: {python_runtime['python_version']}; Paramiko version: {python_runtime['paramiko_version']}; ABI: {python_runtime['abi']}.\n"
               if python_runtime is not None else
               'The Paramiko gateway helper source is bundled; Python/Paramiko runtime is not bundled and is required only for explicitly configured gateway use.\n')
            + 'External AO/Reason and model access are separate dependencies and are not bundled.\n'
            'This package is unsigned; Windows/SmartScreen may show warnings. No signature claim.\n'
            'This package does not install a service, change PATH, or migrate old state.\n'
            'Closing the GUI is not evidence that a remote worker was cancelled.\n'
            'Linux build/native launch is outside this packaging-only operation and is not verified here.\n'
            'Native package/window verification is tracked separately from package creation.\n'
            'Dependency license metadata and available original texts are included.\n', encoding='utf-8')
        verify_sources(root, files)
        for binary, spec in identity_executables.items():
            require(digest(output / binary) == spec['sha256'] and (output / binary).stat().st_size == spec['bytes'], 'Packaged executable identity drift: ' + binary)
        for relative in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml', 'packaging/protocol-v1.md', 'README.md'):
            require(digest(output / 'source-identity' / relative) == files[relative]['sha256'], 'Packaged source identity drift: ' + relative)
        require(digest(output / 'README.md') == files['README.md']['sha256'], 'Packaged README identity drift')
        record['license_text_gaps'] = [entry['name'] + '-' + entry['version'] for entry in record['dependencies'] if not entry['license_files']]
        record['files'] = {path.relative_to(output).as_posix(): {'bytes': path.stat().st_size, 'sha256': digest(path)} for path in sorted(output.rglob('*')) if path.is_file()}
        record['status'] = 'PACKAGE_CREATED_RUNTIME_NOT_YET_VERIFIED'
        with (output / 'PACKAGE-MANIFEST.json').open('x', encoding='utf-8') as stream:
            json.dump(record, stream, ensure_ascii=False, indent=2)
        with zipfile.ZipFile(archive, 'x', compression=zipfile.ZIP_DEFLATED) as bundle:
            for path in sorted(output.rglob('*')):
                if path.is_file():
                    bundle.write(path, Path(name) / path.relative_to(output))
        print(json.dumps({'status': record['status'], 'source_sha256': source_digest, 'package_dir': str(output), 'zip': str(archive), 'zip_sha256': digest(archive), 'manifest_sha256': digest(output / 'PACKAGE-MANIFEST.json'), 'license_text_gaps': record['license_text_gaps'], 'runtime_dll_count': len(record['runtime_dlls'])}))
    except Exception as error:
        record.update(status='PACKAGE_FAILED_PARTIAL_RETAINED', error=type(error).__name__ + ': ' + str(error))
        with (output / 'PACKAGE-FAILURE.json').open('x', encoding='utf-8') as stream:
            json.dump(record, stream, ensure_ascii=False, indent=2)
        raise


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f'{type(error).__name__}: {error}', file=sys.stderr)
        raise SystemExit(2)
