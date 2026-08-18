# acadctl

`acadctl` is a tool for controlling AutoCAD by running AutoLISP via CLI.

This is an early stage, be ready to use AI agent for some additional coding and configuration.

Runs on MacOS, contributions for Windows are welcome.

Examples:

```sh
$ acadctl list
> 6A8436C8  *  rw  /path/to/foo.dwg
  6A8491B2  .  ro  /path/to/bar.dwg
```

`foo.dwg` (`6A8436C8`) is active and contains unsaved changes while `bar.dwg` (`6A8491B2`) is open read-only and contains no unsaved changes. They're open in the same AutoCAD instance (6A84).

Execution requires the target drawing to be active. Use `acadctl switch TARGET` to activate it persistently, or pass `--force` to `exec`, `eval`, `undo`, or `redo` to activate it temporarily and restore the previously active drawing afterward. This temporarily steals AutoCAD's document focus and may disrupt interactive work.

```sh
$ acadctl exec 6A8436C8 <<'LISP'
(defun square (x)
  (* x x))
LISP
```

```sh
$ acadctl eval 6A8436C8 '(square 7)'
49
```

## What works

- `acadctl list`: list drawings currently open in AutoCAD.
- `acadctl open`, `acadctl save`, `acadctl close`
- `acadctl switch`: make a drawing active.
- `acadctl undo` and `acadctl redo`
- `acadctl exec` and `acadctl eval` for running AutoLISP.
- Basic utilities:
  - `(actl:print value)`: print the value to the console
  - `(actl:println "FOO")`: print a string followed by a newline

## Build on macOS

Dependencies:

- AutoCAD and the matching ObjectARX SDK
- Rust with `cargo`
- Xcode Command Line Tools

Make `cargo` available on `PATH`.

```sh
bin/build release
bin/install
```

`bin/build` writes artifacts to `out/`, and `bin/install` installs the plugin and CLI.

The defaults are AutoCAD 2027 and ObjectARX 2027. Override their paths if needed:

```sh
ACAD_DIR=/path/to/AutoCAD.app ACAD_SDK_DIR=/path/to/ObjectARX bin/build release
ACAD_PLUGIN_DIR=/path/to/plugins ACADCTL_BIN_DIR=/path/to/bin bin/install
```

## License

This project is licensed under the [MIT License].

[MIT License]: LICENSE
