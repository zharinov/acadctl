Don't use IPC protocol versioning. It's early stage, you should not care about backward compatibility.

Keep C++ and AutoLISP surface minimal. It must be just interface bridge boilerplate while the load bearing code is Rust.

When asked to commit, use semantic commit convention.

During development, interact with AutoCAD via `acadctl` (without sandbox). Don't acess AutoCAD directly (via computer use) without explicit approval. Files in the `./tmp` folder are disposable, never ask my permission to modify them or discard unsaved changes. Assume AutoCAD process is disposable and nobody is doing anything important in AutoCAD during you work on the `acadctl`.
