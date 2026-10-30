"""Lazy platform process boundary. Never substitutes an unowned Linux subprocess."""
import os


class ExecutionUnavailable(RuntimeError):
    pass


def JobProcess(argv, cwd, env=None, input_data=b'', output_limit=8 * 1024 * 1024,
        stdout_observer=None, wait_observer=None):
    if os.name != 'nt':
        if __package__:
            from .linux_process import LinuxProcess, active_context
        else:
            from linux_process import LinuxProcess, active_context
        try:
            active_context()
        except (OSError, ValueError) as error:
            raise ExecutionUnavailable('Verified Linux owned host context is required') from error
        return LinuxProcess(argv, cwd, env=env, input_data=input_data, output_limit=output_limit,
            stdout_observer=stdout_observer, wait_observer=wait_observer)
    try:
        from .winjob import JobProcess as WindowsJobProcess
    except ImportError:
        from winjob import JobProcess as WindowsJobProcess
    return WindowsJobProcess(argv, cwd, env=env, input_data=input_data, output_limit=output_limit,
        stdout_observer=stdout_observer, wait_observer=wait_observer)
