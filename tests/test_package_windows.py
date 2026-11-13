import hashlib
import json
import time
from importlib import metadata
from pathlib import Path
import subprocess
import sys
import unittest
import uuid
import zipfile


EVIDENCE_ROOT = Path(__file__).resolve().parents[1] / "dist" / ("pk-" + uuid.uuid4().hex[:6])
EVIDENCE_ROOT.mkdir(parents=True, exist_ok=False)


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "packaging" / "package_windows.py"


class WindowsPackagingCliTests(unittest.TestCase):
    def setUp(self):
        self.work = EVIDENCE_ROOT / ("case-" + uuid.uuid4().hex[:10])
        self.work.mkdir(parents=True, exist_ok=False)
        self.source = self.work / "source"
        self.inventory = self.work / "SOURCE-INVENTORY.json"
        self.metadata = self.work / "CARGO-METADATA.json"
        self.build_identity = self.work / "BUILD-IDENTITY.json"
        self.binary_dir = self.work / "release"
        self.runtime_dir = self.work / "mingw" / "bin"
        self.system_dir = self.work / "Windows" / "System32"
        self.output_parent = self.work / "dist" / "evidence"
        self._make_fixture()

    def _make_fixture(self):
        for relative, content in {
            "Cargo.toml": "[package]\nname = 'agentranch'\nversion = '0.2.0'\n",
            "Cargo.lock": "version = 3\n\n[[package]]\nname = 'agentranch'\nversion = '0.2.0'\n\n[[package]]\nname = 'fixture-dependency'\nversion = '1.0.0'\nsource = 'registry+fixture'\n",
            "rust-toolchain.toml": "[toolchain]\nchannel = '1.99.1'\n",
            "packaging/protocol-v1.md": "fixture protocol\n",
            "README.md": "# Synthetic fixture, not a product\n",
        }.items():
            path = self.source / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")

        inventory = {"files": {}}
        for relative in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "packaging/protocol-v1.md", "README.md"):
            path = self.source / relative
            inventory["files"][relative] = {
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "bytes": path.stat().st_size,
            }
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")

        crate = self.work / "crate"
        crate.mkdir()
        (crate / "Cargo.toml").write_text("[package]\nname = 'fixture-dependency'\n", encoding="utf-8")
        (crate / "LICENSE").write_text("fixture license\n", encoding="utf-8")
        package_id = "fixture-dependency 1.0.0 (registry+fixture)"
        metadata = {
            "workspace_root": str(self.source),
            "resolve": {"nodes": [{"id": package_id}]},
            "packages": [{
                "id": package_id,
                "name": "fixture-dependency",
                "version": "1.0.0",
                "license": "MIT",
                "source": "registry+fixture",
                "manifest_path": str(crate / "Cargo.toml"),
            }],
        }
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")

        self.binary_dir.mkdir()
        for binary in ("agentranch.exe", "aw-cli.exe"):
            (self.binary_dir / binary).write_bytes(b"MZ" + binary.encode("ascii"))
        canonical = json.dumps(inventory["files"], sort_keys=True, separators=(",", ":")).encode("utf-8")
        self.build_identity.write_text(json.dumps({
            "source_sha256": hashlib.sha256(canonical).hexdigest(),
            "metadata_sha256": hashlib.sha256(self.metadata.read_bytes()).hexdigest(),
            "target": "x86_64-pc-windows-gnu",
            "toolchain": "1.99.1",
            "profile": "release",
            "package_version": "0.2.0",
            "executables": {
                binary: {
                    "sha256": hashlib.sha256((self.binary_dir / binary).read_bytes()).hexdigest(),
                    "bytes": (self.binary_dir / binary).stat().st_size,
                }
                for binary in ("agentranch.exe", "aw-cli.exe")
            },
        }), encoding="utf-8")
        self.system_dir.mkdir(parents=True)
        (self.system_dir / "KERNEL32.dll").write_bytes(b"system fixture")
        self.runtime_dir.mkdir(parents=True)
        self.output_parent.mkdir(parents=True)

        objdump = self.work / "fake-objdump.py"
        objdump.write_text(
            "import sys\n"
            "print('DLL Name: KERNEL32.dll')\n",
            encoding="utf-8",
        )
        launcher = self.work / "fake-objdump.cmd"
        python = str(Path(sys.executable))
        launcher.write_text(
            f'@echo off\r\n"{python}" -I "{objdump}" %*\r\n',
            encoding="utf-8",
        )
        self.objdump = launcher

    def _command(self, *extra):
        return [
            sys.executable,
            "-I",
            str(SCRIPT),
            "--source-root",
            str(self.source),
            "--inventory",
            str(self.inventory),
            "--metadata",
            str(self.metadata),
            "--build-identity",
            str(self.build_identity),
            "--binary-dir",
            str(self.binary_dir),
            "--profile",
            "release",
            "--output-parent",
            str(self.output_parent),
            "--runtime-dir",
            str(self.runtime_dir),
            "--objdump",
            str(self.objdump),
            "--system-dir",
            str(self.system_dir),
            *extra,
        ]

    def _run(self, *extra, timeout=30):
        command = self._command(*extra)
        source_before = hashlib.sha256(self.inventory.read_bytes()).hexdigest() if self.inventory.exists() else "MISSING"
        started = time.perf_counter()
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=timeout)
        source_after = hashlib.sha256(self.inventory.read_bytes()).hexdigest() if self.inventory.exists() else "MISSING"
        evidence = {
            "test": self.id(), "argv": command, "cwd": str(ROOT),
            "wait": f"foreground subprocess.run; timeout={timeout} seconds",
            "elapsed_seconds": time.perf_counter() - started,
            "exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr,
            "source_inventory_before_sha256": source_before,
            "source_inventory_after_sha256": source_after,
            "source_unchanged": source_before == source_after,
            "scope": "synthetic fixture only; not a product package or native acceptance",
        }
        (self.work / ("command-" + uuid.uuid4().hex[:8] + ".json")).write_text(
            json.dumps(evidence, indent=2), encoding="utf-8")
        return result

    def _add_source_file(self, relative, content, *, version=None, license_name=None):
        path = self.source / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        spec = {"sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content)}
        if version is not None:
            spec["version"] = version
        if license_name is not None:
            spec["license"] = license_name
        inventory["files"][relative] = spec
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        canonical = json.dumps(inventory["files"], sort_keys=True, separators=(",", ":")).encode("utf-8")
        identity["source_sha256"] = hashlib.sha256(canonical).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")

    def _make_python_runtime_fixture(self):
        runtime = self.work / "python-runtime"
        runtime.mkdir()
        # Non-executable synthetic PE header: static x64 machine/PE32+ checks only.
        header = bytearray(90)
        header[:2] = b"MZ"
        header[60:64] = (64).to_bytes(4, "little")
        header[64:68] = b"PE\0\0"
        header[68:70] = (0x8664).to_bytes(2, "little")
        header[88:90] = (0x20b).to_bytes(2, "little")
        fixture_files = {
            "python.exe": bytes(header) + b"SYNTHETIC-NEVER-EXECUTE",
            "python311.dll": bytes(header) + b"SYNTHETIC-PYTHON-DLL",
            "python311.zip": b"SYNTHETIC-STDLIB-NOT-A-REAL-RUNTIME",
            "Lib/site-packages/paramiko/__init__.py": b"raise RuntimeError('synthetic module must not be executed')\n",
            "licenses/PYTHON.txt": b"Synthetic PSF-2.0 license fixture only\n",
            "licenses/PARAMIKO.txt": b"Synthetic LGPL-2.1-only license fixture only\n",
        }
        entries = {}
        for relative, content in fixture_files.items():
            path = runtime / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            entries[relative] = {
                "sha256": hashlib.sha256(content).hexdigest(),
                "bytes": len(content),
                "version": "3.11.9" if relative != "Lib/site-packages/paramiko/__init__.py" else "4.0.0-fixture",
                "license": "PSF-2.0" if relative != "Lib/site-packages/paramiko/__init__.py" else "LGPL-2.1-only",
            }
        inventory_path = self.work / "PYTHON-RUNTIME-INVENTORY.json"
        helper_content = b"# synthetic gateway helper fixture\n"
        self._add_source_file(
            "tools/paramiko_gateway.py", helper_content,
            version="gateway-helper-1", license_name="MIT",
        )
        inventory_path.write_text(json.dumps({
            "schema": "ao-windows-python-runtime/1",
            "platform": "windows-x86_64",
            "abi": "cp311-win_amd64",
            "python_version": "3.11.9",
            "paramiko_version": "4.0.0-fixture",
            "files": entries,
            "helper": {
                "relative": "tools/paramiko_gateway.py",
                "sha256": hashlib.sha256(helper_content).hexdigest(),
                "bytes": len(helper_content),
                "version": "gateway-helper-1",
                "license": "MIT",
            },
        }), encoding="utf-8")
        return runtime, inventory_path

    def _make_installed_cp314_runtime_fixture(self):
        if sys.version_info[:3] != (3, 14, 3):
            self.skipTest("Requires the installed CPython 3.14.3 runtime contract")
        try:
            paramiko = metadata.distribution("paramiko")
        except metadata.PackageNotFoundError:
            self.skipTest("Requires installed Paramiko 4.0.0")
        if paramiko.version != "4.0.0":
            self.skipTest("Requires installed Paramiko 4.0.0")
        runtime = self.work / "python-runtime-cp314"
        prefix = Path(sys.executable).parent
        sources = {relative: prefix / relative for relative in (
            "python.exe", "python314.dll", "Lib/os.py",
        )}
        sources["Lib/site-packages/paramiko/__init__.py"] = Path(
            paramiko.locate_file("paramiko/__init__.py"))
        entries = {}
        for relative, source in sources.items():
            content = source.read_bytes()
            destination = runtime / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(content)
            is_paramiko = relative.startswith("Lib/site-packages/")
            entries[relative] = {
                "sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content),
                "version": "4.0.0" if is_paramiko else "3.14.3",
                "license": "LGPL-2.1" if is_paramiko else "PSF-2.0",
            }
        helper_content = b"# synthetic gateway helper fixture\n"
        self._add_source_file("tools/paramiko_gateway.py", helper_content)
        inventory = self.work / "PYTHON-CP314-RUNTIME-INVENTORY.json"
        inventory.write_text(json.dumps({
            "schema": "ao-windows-python-runtime/1", "platform": "windows-x86_64",
            "abi": "cp314-win_amd64", "python_version": "3.14.3", "paramiko_version": "4.0.0",
            "files": entries, "helper": {
                "relative": "tools/paramiko_gateway.py",
                "sha256": hashlib.sha256(helper_content).hexdigest(), "bytes": len(helper_content),
                "version": "gateway-helper-1", "license": "MIT",
            },
        }), encoding="utf-8")
        return runtime, inventory

    def test_installed_cp314_runtime_accepts_lib_layout_without_stdlib_zip(self):
        runtime, inventory = self._make_installed_cp314_runtime_fixture()
        self.assertFalse((runtime / "python314.zip").exists())
        result = self._run("--python-runtime-dir", str(runtime),
                           "--python-runtime-inventory", str(inventory))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        package = Path(json.loads(result.stdout)["package_dir"])
        manifest = json.loads((package / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
        contract = manifest["python_runtime"]
        self.assertEqual(contract["abi"], "cp314-win_amd64")
        self.assertEqual(contract["python_version"], "3.14.3")
        self.assertEqual(contract["paramiko_version"], "4.0.0")
        self.assertNotIn("python314.zip", contract["files"])
        for relative, spec in contract["files"].items():
            copied = package / "runtime/python" / relative
            self.assertEqual(copied.read_bytes(), (runtime / relative).read_bytes())
            self.assertEqual(hashlib.sha256(copied.read_bytes()).hexdigest(), spec["sha256"])
        self.assertIn("cp314-win_amd64", (package / "PACKAGE-NOTES.txt").read_text(encoding="utf-8"))

    def test_installed_cp314_runtime_rejects_cp311_version(self):
        runtime, inventory = self._make_installed_cp314_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["python_version"] = "3.11.9"
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "version does not match cp314-win_amd64")

    def test_installed_cp314_runtime_requires_its_interpreter_dll(self):
        runtime, inventory = self._make_installed_cp314_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        del data["files"]["python314.dll"]
        (runtime / "python314.dll").unlink()
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "missing required file: python314.dll")

    def test_installed_cp314_runtime_checks_interpreter_dll_architecture(self):
        runtime, inventory = self._make_installed_cp314_runtime_fixture()
        dll = runtime / "python314.dll"
        raw = bytearray(dll.read_bytes())
        pe_offset = int.from_bytes(raw[60:64], "little")
        raw[pe_offset + 4:pe_offset + 6] = (0x14c).to_bytes(2, "little")
        dll.write_bytes(raw)
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["files"]["python314.dll"]["sha256"] = hashlib.sha256(raw).hexdigest()
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "runtime PE architecture mismatch: python314.dll")

    def test_bundled_runtime_is_copied_and_hash_bound_without_launching_runtime(self):
        runtime, inventory = self._make_python_runtime_fixture()
        result = self._run(
            "--python-runtime-dir", str(runtime),
            "--python-runtime-inventory", str(inventory),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(result.stdout)
        package_dir = Path(summary["package_dir"])
        manifest = json.loads((package_dir / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
        runtime_manifest = manifest["python_runtime"]
        self.assertEqual(runtime_manifest["abi"], "cp311-win_amd64")
        self.assertEqual(runtime_manifest["python_version"], "3.11.9")
        self.assertEqual(runtime_manifest["paramiko_version"], "4.0.0-fixture")
        for relative, spec in runtime_manifest["files"].items():
            packaged = package_dir / "runtime" / "python" / relative
            self.assertTrue(packaged.is_file(), relative)
            self.assertEqual(hashlib.sha256(packaged.read_bytes()).hexdigest(), spec["sha256"])
            self.assertEqual(packaged.stat().st_size, spec["bytes"])
            self.assertTrue(spec["version"])
            self.assertTrue(spec["license"])
        helper = package_dir / "tools" / "paramiko_gateway.py"
        self.assertEqual(helper.read_bytes(), b"# synthetic gateway helper fixture\n")
        self.assertIn("Bundled Python runtime", (package_dir / "PACKAGE-NOTES.txt").read_text(encoding="utf-8"))
        self.assertIn("cp311-win_amd64", (package_dir / "PACKAGE-NOTES.txt").read_text(encoding="utf-8"))
        self.assertIn("4.0.0-fixture", (package_dir / "PACKAGE-NOTES.txt").read_text(encoding="utf-8"))

    def test_bundled_runtime_requires_interpreter_before_output(self):
        runtime, inventory = self._make_python_runtime_fixture()
        (runtime / "python.exe").unlink()
        result = self._run(
            "--python-runtime-dir", str(runtime),
            "--python-runtime-inventory", str(inventory),
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Bundled Python runtime file missing: python.exe", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_bundled_runtime_rejects_path_escape_before_output(self):
        runtime, inventory = self._make_python_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["files"]["../outside.dll"] = data["files"].pop("python311.dll")
        inventory.write_text(json.dumps(data), encoding="utf-8")
        result = self._run(
            "--python-runtime-dir", str(runtime),
            "--python-runtime-inventory", str(inventory),
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("Unsafe runtime inventory path", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def _assert_runtime_preflight_failure(self, runtime, inventory, message):
        result = self._run(
            "--python-runtime-dir", str(runtime),
            "--python-runtime-inventory", str(inventory),
        )
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(message, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertEqual(list(self.output_parent.iterdir()), [])
        self.assertTrue(list(self.work.glob("command-*.json")), "retain failure command evidence")

    def test_bundled_runtime_rejects_wrong_abi(self):
        runtime, inventory = self._make_python_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["abi"] = "cp311-win32"
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "runtime ABI mismatch")

    def test_bundled_runtime_rejects_hash_and_bytes_drift_independently(self):
        runtime, inventory = self._make_python_runtime_fixture()
        original = json.loads(inventory.read_text(encoding="utf-8"))
        for field, invalid in [("sha256", "0" * 64), ("bytes", 1)]:
            with self.subTest(field=field):
                data = json.loads(json.dumps(original))
                data["files"]["python.exe"][field] = invalid
                inventory.write_text(json.dumps(data), encoding="utf-8")
                self._assert_runtime_preflight_failure(runtime, inventory, "Python runtime inventory drift")

    def test_bundled_runtime_rejects_version_abi_mismatch(self):
        runtime, inventory = self._make_python_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["python_version"] = "3.12.1"
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "runtime ABI mismatch")

    def test_bundled_runtime_rejects_x86_interpreter_even_with_matching_hash(self):
        runtime, inventory = self._make_python_runtime_fixture()
        interpreter = runtime / "python.exe"
        raw = bytearray(interpreter.read_bytes())
        raw[68:70] = (0x14c).to_bytes(2, "little")
        interpreter.write_bytes(raw)
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["files"]["python.exe"]["sha256"] = hashlib.sha256(raw).hexdigest()
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "runtime PE architecture mismatch")

    def test_bundled_runtime_rejects_missing_license_or_version(self):
        runtime, inventory = self._make_python_runtime_fixture()
        original = json.loads(inventory.read_text(encoding="utf-8"))
        for field in ["license", "version"]:
            with self.subTest(field=field):
                data = json.loads(json.dumps(original))
                del data["files"]["python.exe"][field]
                inventory.write_text(json.dumps(data), encoding="utf-8")
                self._assert_runtime_preflight_failure(runtime, inventory, f"file {field} is required")

    def test_bundled_runtime_rejects_helper_identity_mismatch(self):
        runtime, inventory = self._make_python_runtime_fixture()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        data["helper"]["sha256"] = "0" * 64
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "helper identity mismatch")

    def test_bundled_runtime_rejects_unlisted_files_and_credential_paths(self):
        runtime, inventory = self._make_python_runtime_fixture()
        extra = runtime / "unlisted.txt"
        extra.write_bytes(b"synthetic unlisted")
        self._assert_runtime_preflight_failure(runtime, inventory, "inventory closure mismatch")
        extra.unlink()
        data = json.loads(inventory.read_text(encoding="utf-8"))
        # Do not create or read a credential file; reject the path before opening.
        data["files"]["known_hosts"] = dict(data["files"]["python.exe"])
        inventory.write_text(json.dumps(data), encoding="utf-8")
        self._assert_runtime_preflight_failure(runtime, inventory, "Forbidden runtime credential path")

    def test_bundled_runtime_rejects_windows_path_aliases(self):
        runtime, inventory = self._make_python_runtime_fixture()
        original = json.loads(inventory.read_text(encoding="utf-8"))
        for relative in ["/absolute.dll", "C:/outside.dll", "../outside.dll", "Lib\\outside.dll",
                         "./python.exe", "Lib//outside.dll", "python.exe:stream", "python.exe."]:
            with self.subTest(path=relative):
                data = json.loads(json.dumps(original))
                data["files"][relative] = dict(data["files"]["python.exe"])
                inventory.write_text(json.dumps(data), encoding="utf-8")
                self._assert_runtime_preflight_failure(runtime, inventory, "Unsafe runtime inventory path")

    def test_bundled_runtime_detects_copy_time_drift_without_success_artifacts(self):
        runtime, inventory = self._make_python_runtime_fixture()
        self.work.joinpath("fake-objdump.py").write_text(
            "from pathlib import Path\n"
            f"Path({str(runtime / 'python.exe')!r}).write_bytes(b'SYNTHETIC-DRIFT')\n"
            "print('DLL Name: KERNEL32.dll')\n", encoding="utf-8")
        result = self._run("--python-runtime-dir", str(runtime), "--python-runtime-inventory", str(inventory))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertFalse(list(self.output_parent.glob("*.zip")))
        self.assertFalse(list(self.output_parent.rglob("PACKAGE-MANIFEST.json")))
        self.assertTrue(list(self.output_parent.rglob("PACKAGE-FAILURE.json")))

    def test_existing_zip_destination_is_rejected_without_overwrite(self):
        files = json.loads(self.inventory.read_text(encoding="utf-8"))["files"]
        source_digest = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(",", ":")).encode("utf-8")).hexdigest()
        archive = self.output_parent / f"agentranch-0.2.0-windows-x86_64-release-{source_digest[:16]}.zip"
        archive.write_bytes(b"prior evidence")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Package destination already exists", result.stderr)
        self.assertEqual(archive.read_bytes(), b"prior evidence")

    def test_fixture_package_reports_identity_and_manifest_hashes(self):
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        summary = json.loads(result.stdout)
        package_dir = Path(summary["package_dir"])
        archive = Path(summary["zip"])
        manifest = json.loads((package_dir / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))

        self.assertEqual(manifest["status"], "PACKAGE_CREATED_RUNTIME_NOT_YET_VERIFIED")
        self.assertEqual(set(manifest["executables"]), {"agentranch.exe", "aw-cli.exe"})
        self.assertTrue((package_dir / "README.md").is_file())
        self.assertEqual((package_dir / "BUILD-IDENTITY.json").read_bytes(), self.build_identity.read_bytes())
        for binary, spec in manifest["executables"].items():
            self.assertEqual(hashlib.sha256((package_dir / binary).read_bytes()).hexdigest(), spec["sha256"])
        self.assertEqual(summary["manifest_sha256"], hashlib.sha256((package_dir / "PACKAGE-MANIFEST.json").read_bytes()).hexdigest())
        with zipfile.ZipFile(archive) as bundle:
            names = {name.split("/", 1)[1] for name in bundle.namelist()}
            self.assertEqual(names, {path.relative_to(package_dir).as_posix() for path in package_dir.rglob("*") if path.is_file()})
            for relative, spec in manifest["files"].items():
                self.assertEqual(hashlib.sha256((package_dir / relative).read_bytes()).hexdigest(), spec["sha256"])
                self.assertEqual(bundle.read(f"{package_dir.name}/{relative}"), (package_dir / relative).read_bytes())

    def test_missing_executable_is_a_clean_cli_failure(self):
        (self.binary_dir / "aw-cli.exe").unlink()
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Release PE missing: aw-cli.exe", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_missing_import_dll_retains_failure_evidence_without_manifest_claim(self):
        (self.system_dir / "KERNEL32.dll").unlink()
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Unresolved PE dependency: KERNEL32.dll", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        partial = list(self.output_parent.iterdir())
        self.assertEqual(len(partial), 1)
        self.assertTrue((partial[0] / "PACKAGE-FAILURE.json").is_file())

    def test_objdump_timeout_is_a_clean_failure_with_retained_evidence(self):
        self.work.joinpath("fake-objdump.py").write_text(
            "import time\n"
            "time.sleep(31)\n",
            encoding="utf-8",
        )

        result = self._run(timeout=35)

        self.assertEqual(result.returncode, 2)
        self.assertIn("PE import inspection timed out: agentranch.exe", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        partial = list(self.output_parent.iterdir())
        self.assertEqual(len(partial), 1)
        failure = partial[0] / "PACKAGE-FAILURE.json"
        self.assertTrue(failure.is_file())
        self.assertIn("RuntimeError: PE import inspection timed out: agentranch.exe", failure.read_text(encoding="utf-8"))

    def test_missing_inventory_is_a_clean_input_failure(self):
        self.inventory.unlink()
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Input path missing", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_existing_destination_is_rejected_without_overwrite(self):
        files = json.loads(self.inventory.read_text(encoding="utf-8"))["files"]
        canonical = json.dumps(files, sort_keys=True, separators=(",", ":")).encode("utf-8")
        source_digest = hashlib.sha256(canonical).hexdigest()
        existing = self.output_parent / f"agentranch-0.2.0-windows-x86_64-release-{source_digest[:16]}"
        existing.mkdir()
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Package destination already exists", result.stderr)
        self.assertTrue(existing.is_dir())
        self.assertEqual(len(list(self.output_parent.iterdir())), 1)

    def test_distinct_dependency_sources_keep_license_texts_separate(self):
        second_crate = self.work / "second-crate"
        second_crate.mkdir()
        (second_crate / "Cargo.toml").write_text("[package]\nname = 'fixture-dependency'\n", encoding="utf-8")
        (second_crate / "LICENSE").write_text("second fixture license\n", encoding="utf-8")
        second_id = "fixture-dependency 1.0.0 (git+fixture#second)"
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["resolve"]["nodes"].append({"id": second_id})
        metadata["packages"].append({
            "id": second_id,
            "name": "fixture-dependency",
            "version": "1.0.0",
            "license": "MIT",
            "source": "git+fixture#second",
            "manifest_path": str(second_crate / "Cargo.toml"),
        })
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        lock = self.source / "Cargo.lock"
        lock.write_text(lock.read_text(encoding="utf-8") + "\n[[package]]\nname = 'fixture-dependency'\nversion = '1.0.0'\nsource = 'git+fixture#second'\n", encoding="utf-8")
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        inventory["files"]["Cargo.lock"] = {
            "sha256": hashlib.sha256(lock.read_bytes()).hexdigest(), "bytes": lock.stat().st_size,
        }
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        canonical = json.dumps(inventory["files"], sort_keys=True, separators=(",", ":")).encode("utf-8")
        identity["source_sha256"] = hashlib.sha256(canonical).hexdigest()
        identity["metadata_sha256"] = hashlib.sha256(self.metadata.read_bytes()).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")

        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        package_dir = Path(json.loads(result.stdout)["package_dir"])
        manifest = json.loads((package_dir / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
        dependencies = [entry for entry in manifest["dependencies"] if entry["name"] == "fixture-dependency"]
        self.assertEqual(len(dependencies), 2)
        self.assertEqual(
            {entry["source"] for entry in dependencies},
            {"registry+fixture", "git+fixture#second"},
        )
        preserved = {
            (package_dir / entry["license_files"][0]["file"]).read_text(encoding="utf-8")
            for entry in dependencies
        }
        self.assertEqual(preserved, {"fixture license\n", "second fixture license\n"})
        with zipfile.ZipFile(Path(json.loads(result.stdout)["zip"])) as bundle:
            for entry in dependencies:
                relative = entry["license_files"][0]["file"]
                self.assertEqual(bundle.read(f"{package_dir.name}/{relative}"), (package_dir / relative).read_bytes())

    def test_nested_font_license_texts_are_preserved(self):
        (self.work / "crate" / "LICENSE").unlink()
        fonts = self.work / "crate" / "fonts"
        fonts.mkdir()
        expected = {
            "OFL.txt": "SIL Open Font License 1.1.\n",
            "UFL.txt": "Ubuntu Font Licence 1.0.\n",
        }
        for name, content in expected.items():
            (fonts / name).write_text(content, encoding="utf-8")

        result = self._run()

        self.assertEqual(result.returncode, 0, result.stderr)
        package_dir = Path(json.loads(result.stdout)["package_dir"])
        manifest = json.loads((package_dir / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
        dependency = manifest["dependencies"][0]
        self.assertEqual(
            {Path(entry["file"]).name for entry in dependency["license_files"]},
            set(expected),
        )
        self.assertEqual(dependency["license_text_status"], "INCLUDED")
        self.assertEqual(
            {
                (package_dir / entry["file"]).read_text(encoding="utf-8")
                for entry in dependency["license_files"]
            },
            set(expected.values()),
        )

    def test_runtime_dll_is_copied_and_hashed_when_imported(self):
        runtime = self.runtime_dir / "libgcc_s_seh-1.dll"
        runtime.write_bytes(b"runtime fixture")
        (self.work / "fake-objdump.py").write_text(
            "print('DLL Name: KERNEL32.dll')\n"
            "print('DLL Name: libgcc_s_seh-1.dll')\n",
            encoding="utf-8",
        )
        result = self._run()
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(result.stdout)
        package_dir = Path(summary["package_dir"])
        manifest = json.loads((package_dir / "PACKAGE-MANIFEST.json").read_text(encoding="utf-8"))
        self.assertEqual(summary["runtime_dll_count"], 1)
        self.assertEqual(manifest["runtime_dlls"][0]["name"], "libgcc_s_seh-1.dll")
        self.assertEqual((package_dir / "libgcc_s_seh-1.dll").read_bytes(), runtime.read_bytes())
        self.assertEqual(manifest["files"]["libgcc_s_seh-1.dll"]["sha256"], hashlib.sha256(runtime.read_bytes()).hexdigest())

    def test_runtime_dll_symlink_outside_approved_directory_is_rejected(self):
        runtime = self.runtime_dir / "libgcc_s_seh-1.dll"
        outside = self.work / "outside-runtime.dll"
        outside.write_bytes(b"outside runtime fixture")
        try:
            runtime.symlink_to(outside)
        except (OSError, NotImplementedError) as error:
            self.skipTest("Windows symlink creation unavailable: " + str(error))
        (self.work / "fake-objdump.py").write_text(
            "print('DLL Name: KERNEL32.dll')\n"
            "print('DLL Name: libgcc_s_seh-1.dll')\n",
            encoding="utf-8",
        )

        result = self._run()

        self.assertEqual(result.returncode, 2)
        self.assertIn("Runtime DLL is outside approved runtime directory: libgcc_s_seh-1.dll", result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        partial = list(self.output_parent.iterdir())
        self.assertEqual(len(partial), 1)
        self.assertTrue((partial[0] / "PACKAGE-FAILURE.json").is_file())
        self.assertFalse((partial[0] / "libgcc_s_seh-1.dll").exists())

    def test_build_identity_rejects_manifest_toolchain_mismatch_before_output(self):
        self.source.joinpath("rust-toolchain.toml").write_text("[toolchain]\nchannel = '1.98.0'\n", encoding="utf-8")
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        path = self.source / "rust-toolchain.toml"
        inventory["files"]["rust-toolchain.toml"] = {
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "bytes": path.stat().st_size,
        }
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        canonical = json.dumps(inventory["files"], sort_keys=True, separators=(",", ":")).encode("utf-8")
        identity["source_sha256"] = hashlib.sha256(canonical).hexdigest()
        identity["metadata_sha256"] = hashlib.sha256(self.metadata.read_bytes()).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Build identity toolchain mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_build_identity_rejects_executable_drift_before_output(self):
        (self.binary_dir / "aw-cli.exe").write_bytes(b"MZdifferent build")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Executable identity drift: aw-cli.exe", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_metadata_resolved_package_must_exist_in_frozen_lock_before_output(self):
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        metadata["packages"][0]["source"] = "registry+different-fixture"
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        identity["metadata_sha256"] = hashlib.sha256(self.metadata.read_bytes()).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Cargo metadata package mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_metadata_resolved_node_must_have_package_record_before_output(self):
        metadata = json.loads(self.metadata.read_text(encoding="utf-8"))
        missing_id = metadata["resolve"]["nodes"][0]["id"]
        metadata["packages"] = []
        self.metadata.write_text(json.dumps(metadata), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        identity["metadata_sha256"] = hashlib.sha256(self.metadata.read_bytes()).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")

        result = self._run()

        self.assertEqual(result.returncode, 2)
        self.assertIn("Cargo metadata resolved node missing package record: " + missing_id, result.stderr)
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_build_identity_rejects_manifest_package_version_mismatch_before_output(self):
        # Rewrite the manifest to a DIFFERENT version than the pinned 0.2.0 so
        # the mismatch check still exercises rejection (not a no-op rewrite).
        path = self.source / "Cargo.toml"
        path.write_text("[package]\nname = 'agentranch'\nversion = '9.9.9'\n", encoding="utf-8")
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        inventory["files"]["Cargo.toml"] = {
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "bytes": path.stat().st_size,
        }
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        canonical = json.dumps(inventory["files"], sort_keys=True, separators=(",", ":")).encode("utf-8")
        identity["source_sha256"] = hashlib.sha256(canonical).hexdigest()
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Build identity package version mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_build_identity_rejects_metadata_drift_before_output(self):
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        identity["metadata_sha256"] = "0" * 64
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Build identity metadata mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_build_identity_rejects_other_source_before_output(self):
        identity = json.loads(self.build_identity.read_text(encoding="utf-8"))
        identity["source_sha256"] = "0" * 64
        self.build_identity.write_text(json.dumps(identity), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Build identity source mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_source_drift_during_inspection_never_reports_success(self):
        (self.work / "fake-objdump.py").write_text(
            "from pathlib import Path\n"
            f"Path({str(self.source / 'README.md')!r}).write_text('inspection drift', encoding='utf-8')\n"
            "print('DLL Name: KERNEL32.dll')\n",
            encoding="utf-8",
        )
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Source inventory drift: README.md", result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertFalse(list(self.output_parent.glob("*.zip")))
        self.assertFalse(list(self.output_parent.rglob("PACKAGE-MANIFEST.json")))

    def test_all_copied_source_identity_files_must_be_inventory_bound(self):
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        del inventory["files"]["rust-toolchain.toml"]
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Incomplete source inventory", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_source_drift_is_rejected_before_output(self):
        (self.source / "README.md").write_text("changed source", encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Source inventory drift: README.md", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_metadata_drift_during_inspection_is_rejected_before_success(self):
        self.work.joinpath("fake-objdump.py").write_text(
            "from pathlib import Path\n"
            f"metadata = Path({str(self.metadata)!r})\n"
            "metadata.write_bytes(metadata.read_bytes() + b' ')\n"
            "print('DLL Name: KERNEL32.dll')\n",
            encoding="utf-8",
        )
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Packaged metadata identity drift", result.stderr)
        self.assertEqual(result.stdout, "")
        partial = list(self.output_parent.iterdir())
        self.assertEqual(len(partial), 1)
        self.assertTrue((partial[0] / "PACKAGE-FAILURE.json").is_file())
        self.assertFalse(list(self.output_parent.glob("*.zip")))
        self.assertFalse(list(self.output_parent.rglob("PACKAGE-MANIFEST.json")))

    def test_unlisted_source_file_is_rejected_before_output(self):
        (self.source / "unlisted-source.txt").write_text("not inventory bound", encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Source inventory closure mismatch", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])

    def test_readme_must_be_inventory_bound(self):
        inventory = json.loads(self.inventory.read_text(encoding="utf-8"))
        del inventory["files"]["README.md"]
        self.inventory.write_text(json.dumps(inventory), encoding="utf-8")
        result = self._run()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Incomplete source inventory", result.stderr)
        self.assertEqual(list(self.output_parent.iterdir()), [])


if __name__ == "__main__":
    print(f"Synthetic fixture evidence: {EVIDENCE_ROOT}", flush=True)
    unittest.main()
