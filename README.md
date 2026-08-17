# acadctl

`acadctl` is a tool for controlling AutoCAD by running AutoLISP via CLI.

This is an early stage, be ready to use AI agent for some additional coding and configuration.

Runs on MacOS, contributions for Windows are welcome.

Examples:

```sh
$ acadctl ps
6A84:36C8  *  rw  foo.dwg
6A84:91B2  .  ro  bar.dwg
```

`foo.dwg` (`6A84:36C8`) contains unsaved changes while `bar.dwg` (`6A84:91B2`) is open read-only and contains no unsaved changes. They're open in the same AutoCAD instance (6A84).

```sh
$ acadctl exec 6A84:36C8 <<'LISP'
(defun square (x)
  (* x x))
LISP
```

```sh
$ acadctl eval 6A84:36C8 '(square 7)'
49
```

## What works

- `acadctl ps`: list drawings currently open in AutoCAD.
- `acadctl open`, `acadctl save`, `acadctl close`
- `acadctl undo` and `acadctl redo`
- `acadctl exec` and `acadctl eval` for running AutoLISP.
- Basic utilities:
  - `(actl:print value)`: print the value to the console
  - `(actl:label "FOO")`: print the `--- FOO ---` label for debug

## Build on macOS

Dependencies:

- AutoCAD and the matching ObjectARX SDK
- Rust with `cargo`
- Xcode Command Line Tools

Make `cargo` available on `PATH`.

```sh
bin/build release
bin/install-plugin
```

Build artifacts are written to `out/`.

The defaults are AutoCAD 2027 and ObjectARX 2027. Override their paths if needed:

```sh
ACAD_DIR=/path/to/AutoCAD.app ACAD_SDK_DIR=/path/to/ObjectARX bin/build release
ACAD_PLUGIN_DIR=/path/to/plugins bin/install-plugin
```

## License

This project is licensed under the [MIT License].

[MIT License]: LICENSE
