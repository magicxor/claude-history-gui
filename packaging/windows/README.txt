Claude History @VERSION@ (claude-history-gui), Windows x86_64
==============================================================

Run claude-history-gui.exe. Nothing needs to be installed:
the Visual C++ runtime DLLs (MSVCP140*.dll, VCRUNTIME140*.dll) sit next to the exe,
and the ONNX Runtime used by semantic search is linked in (DirectML is delay-loaded,
so the app starts on systems without it).

Command line
------------
The exe is a Windows GUI program. Started from cmd.exe or PowerShell it attaches to
the parent console to print --help, --version, errors and the headless commands
(agent ..., --show-dir, --render FILE, picker output). cmd.exe does not wait for
GUI programs, so to use the output in a script, redirect it or run it through
`start /wait`:

    claude-history-gui --help > help.txt
    for /f %i in ('claude-history-gui --show-id') do echo %i

Run `claude-history-gui --help` for every option. Your settings live in
%USERPROFILE%\.config\claude-history\gui.toml (the CLI's config.toml is only read).

Resume and Fork open a new Windows Terminal window (or a console window when
Windows Terminal is not installed); set [gui] terminal in gui.toml to change that.

Licenses: LICENSE (Unlicense) and LICENSE-MIT (the history core derived from claude-history).
