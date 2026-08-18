Don't use IPC protocol versioning. It's early stage, you should not care about backward compatibility.

Keep C++ and AutoLISP surface minimal. It must be just interface bridge boilerplate while the load bearing code is Rust.

When asked to commit, use semantic commit convention.

During development, you have standing approval to:

- Run `bin/install` and replace the installed `acadctl` CLI and plugin.
- Start, stop, restart, and otherwise control AutoCAD through `acadctl`.
- Open, modify, save, close, reopen, or discard drawings under `./tmp`.
- Create, overwrite, move, or delete files under `./tmp`.

Do not ask for confirmation before these actions. Treat AutoCAD processes and all files and unsaved changes under `./tmp` as disposable. This approval does not apply to drawings or files outside `./tmp`. Do not control AutoCAD through computer use without explicit approval.

If drawing close and reopen is sufficient, use it instead of closing and reopening the entire AutoCAD instance.

Use `acadctl exec` to reload the lisp file instead of rebuilding and reinstalling plugin and restarting AutoCAD.

Lisp code conventions:

- Only define global functions for public API and bridge entry points.
- Reserve `actl:_*` and `actl:*bridge-*` for the bridge.
- Keep helpers as local quoted lambdas called with `apply`.
- Don't return or store local lambdas.
- Split code by responsibility without creating fake-private globals.
- Name non-obvious numbers locally.
- Name DXF group codes and AutoCAD sentinels locally.
- Keep bridge event codes in a semantic local table.
