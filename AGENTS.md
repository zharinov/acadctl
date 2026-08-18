Don't use IPC protocol versioning. It's early stage, you should not care about backward compatibility.

Keep C++ and AutoLISP surface minimal. It must be just interface bridge boilerplate while the load bearing code is Rust.

When asked to commit, use semantic commit convention.

During development, interact with AutoCAD via `acadctl` (without sandbox). Don't acess AutoCAD directly (via computer use) without explicit approval. Files in the `./tmp` folder are disposable, never ask my permission to modify them or discard unsaved changes. Assume AutoCAD process is disposable and nobody is doing anything important in AutoCAD during you work on the `acadctl`.

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
