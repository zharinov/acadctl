# `acadctl eval`, `exec`, and controlled history

Status: agreed design. The execution and history commands remain unimplemented until the native proof gates at the end of this document pass.

## Design center

`acadctl` is a document-aware AutoLISP runner for agents. It is not a second AutoCAD command line and does not introduce a new Lisp dialect.

The design has five priorities:

1. Familiar AutoLISP behavior for source, values, definitions, and printing.
2. Exact routing to one open document without disturbing work already happening in AutoCAD.
3. One drawing undo unit per successful mutating request, with drawing rollback on failure or cancellation.
4. Predictable Unix behavior for stdin, files, stdout, stderr, exit status, Ctrl+C, and broken connections.
5. A minimal C++ bridge, with queueing, scanning, state, protocol, formatting, and policy owned by Rust.

AutoLISP remains fully capable code. `acadctl` is not a sandbox. Agent instructions remain responsible for avoiding document lifecycle operations, explicit undo manipulation, saves, cross-document COM changes, and other effects that do not belong in a request.

## Public command line

| Command | Meaning | Successful stdout |
| --- | --- | --- |
| `acadctl eval <id> [file]` | Evaluate exactly one top-level AutoLISP form. | The form's readable value, followed by a newline; explicit `acadctl:println` output comes first. |
| `acadctl exec <id> [file]` | Execute zero or more top-level AutoLISP forms as one batch. | Only explicit `acadctl:println` output. |
| `acadctl undo <id> [--force]` | Undo one drawing-history step. | Nothing. |
| `acadctl redo <id> [--force]` | Redo one drawing-history step. | Nothing. |
| `acadctl kill [pid] [--force]` | Terminate an AutoCAD instance, not an execution request. | Nothing. |

`<id>` is the document ID reported by `acadctl ls`. The document ID also resolves the owning AutoCAD process, matching the existing `save` and `close` commands.

There is no `repl` command. Repeated `eval` or `exec` invocations provide command-by-command use, while one invocation containing several forms provides batch use. Definitions and variables live in the target document's normal AutoLISP environment and therefore persist across requests until that document closes.

There is no public `cancel` command. Ctrl+C belongs to the foreground execution request. `kill` is deliberately separate because it affects an entire AutoCAD process and every document in it.

### Source selection

The optional positional argument is always a file, never inline source.

- No file argument reads stdin.
- `-` explicitly selects stdin.
- One other argument names a local file read by the CLI.
- Extra positional arguments are command-line usage errors.
- A literal file named `-` is addressed as `./-` or by another path containing a separator.

When stdin is a terminal, the CLI prints a short instruction to stderr and collects a complete batch through EOF. It does not become a REPL and does not execute forms as they arrive. Command-by-command use means separate CLI invocations.

The CLI reads the complete source before contacting AutoCAD. This keeps input waiting outside AutoCAD's main loop and makes one invocation the atomic batch boundary.

### Text contract

- Source is UTF-8.
- An optional UTF-8 BOM is removed.
- Invalid UTF-8 fails locally before a request is queued.
- Source is sent as one complete protocol message.
- Source is limited to 4 MiB of UTF-8 bytes after BOM removal.
- The CLI enforces the limit for a fast error; the plugin enforces it authoritatively.
- There is no size override initially. A multi-megabyte AutoLISP batch indicates that the caller should split or redesign the work.
- The file is read by the CLI and sent in memory. AutoCAD does not `load` the path, search its support paths, or reread a file that may have changed.

The source name used in diagnostics is the supplied file path or `<stdin>`.

### Empty input

- Empty `exec` input succeeds as a no-op, produces no output, and creates no undo step.
- Empty `eval` input fails because `eval` requires exactly one form.

## `eval` and `exec` semantics

| Property | `eval` | `exec` |
| --- | --- | --- |
| Accepted top-level forms | Exactly one | Zero or more |
| Implicit values | Prints the form's value, including `nil` | Suppresses every form value |
| Definitions and assignments | Allowed and persistent | Allowed and persistent |
| Drawing mutation | Allowed | Allowed |
| Drawing undo scope | One request | One request across all forms |
| Cancellation checkpoints | Before the form and after it returns | Before the first form and between forms |

Examples of implicit results:

| Form | `eval` stdout | `exec` stdout |
| --- | --- | --- |
| `(setq count 12)` | `12` | Empty |
| `(defun twice (x) (* x 2))` | `TWICE` | Empty |
| `nil` | `nil` | Empty |
| `(acadctl:println "created: " count)` | `created: 12`, then `nil` | `created: 12` |

`eval` is intended for inspection and composition, but it is not read-only. `exec` is intended for definitions, commands, and batches where implicit Lisp return values would be noise.

For `exec`, top-level forms run synchronously and in source order. A failure stops the batch. AutoCAD and other `acadctl` jobs cannot interleave between its forms.

## Top-level form boundaries

ObjectARX exposes no supported general AutoLISP reader or AST API. A small Rust lexical scanner identifies exact byte ranges for top-level forms; AutoLISP remains the authority for reading and evaluating each range.

The scanner recognizes only the lexical structure required to find boundaries:

- Parenthesis depth.
- Strings and backslash escapes.
- `;` comments through end of line.
- `;| ... |;` block comments.
- Top-level atoms and AutoLISP delimiters.
- The apostrophe reader prefix as part of the form that follows it.

It does not build an AST, interpret symbols, validate argument shapes, expand macros, or reproduce AutoLISP evaluation. Its output is declarative:

```rust
struct SourceBatch {
    source_name: String,
    source: String,
    forms: Vec<FormSpan>,
}

struct FormSpan {
    byte_start: usize,
    byte_end: usize,
    line: usize,
    column: usize,
}

enum SourceShape {
    Complete,
    Incomplete { line: usize, column: usize },
    Invalid { line: usize, column: usize },
}
```

`eval` validates that `forms.len()` is exactly one. `exec` accepts any number. Lexically incomplete source fails before admission to AutoCAD. Reader and evaluation errors that require AutoLISP knowledge are reported by the target AutoLISP environment.

Exact form spans are necessary for three reasons: sequential execution, cancellation checks between forms, and diagnostics that identify the failing top-level form.

## Output

### `acadctl:println`

The `acadctl:` symbol prefix is reserved for this tool. The only documented source-output function is:

```lisp
(acadctl:println "created: " count)
```

Its contract is:

- Any number of arguments.
- Arguments are concatenated without an implicit separator.
- Ordinary values use familiar `princ`-style display semantics; strings are not quoted.
- Opaque values use the stable acadctl display forms described below.
- Exactly one newline is added after all arguments.
- The CLI forwards each received line immediately and flushes stdout; buffering does not wait for request completion.
- No arguments produce one blank line.
- Output is routed only to the client owning the active `eval` or `exec` request.
- The function returns `nil`.
- Outside an active request, it has no effect and still returns `nil`.
- After a client disconnects, it has no visible effect and returns `nil` while the accepted job continues.

Standard AutoLISP `princ`, `prin1`, `print`, `prompt`, and related functions are not replaced or captured. They retain their normal AutoCAD command-line behavior. This preserves existing Lisp expectations and keeps the user's AutoCAD console separate from the request's stdout.

No automatic label, prefix, or request ID is added to output. Callers can include their own labels as ordinary arguments.

### Value printers

There are two related printer modes:

- The implicit `eval` result uses readable Lisp-native formatting for ordinary values, analogous to `prin1`.
- `acadctl:println` uses display formatting for ordinary values, analogous to `princ`.

Both modes use stable forms for opaque values, including when an opaque value is nested inside ordinary data:

```text
#<Entity 5A2>
#<SelectionSet 7>
#<VlaObject IAcadLine>
#<File>
#<Function foo>
```

Type names use PascalCase. A payload appears only when it is useful. These forms are displays, not new readable AutoLISP literals.

Only an entity handle is intentionally reusable identity. The entity from `#<Entity 5A2>` can later be resolved with `(handent "5A2")`, subject to the entity still being live in that drawing. Selection-set numbers, COM class names, file displays, and function names are descriptive rather than general object handles.

The implicit `eval` value is emitted only after successful completion of the drawing undo group. If execution or finalization fails, there is no implicit value. Previously streamed `acadctl:println` output remains visible.

### Backpressure

Output is never silently dropped while a client remains connected, and queued output is not allowed to grow without bound. A slow stdout consumer applies backpressure at `acadctl:println`. Ctrl+C wakes a blocked output operation and requests cancellation. A disconnected sink switches to discard mode so the accepted AutoLISP job can finish without blocking or accumulating output.

## Errors and exit status

| Status | Meaning |
| --- | --- |
| `0` | Successful command. |
| `1` | Input, connection, busy, AutoLISP, native, rollback, undo, or redo failure. |
| `2` | Invalid command-line arguments, produced by the CLI parser. |
| `130` | Ctrl+C cancellation or second-Ctrl+C detachment. |

There is no special `75`/`EX_TEMPFAIL` status for a busy document. Although established in BSD `sysexits`, it is not a familiar interpreter convention. A pre-execution busy failure is instead recognizable from its stable human diagnostic and remains safe to retry because no form started.

Source diagnostics follow the concise triaged style used by user-friendly language launchers:

```text
Execution error in script.lsp, form 3 (line 12).
bad argument type: numberp nil
```

Scanner errors have an exact position:

```text
Read error in script.lsp (line 12, column 17).
unterminated string
```

Runtime diagnostics identify the start of the failing top-level form because the native AutoLISP evaluator does not provide a reliable inner-expression source location. The design does not invent a precise column, build a full parser solely for diagnostics, manufacture a stack trace, or create a temporary exception report.

Normal values and explicit output go to stdout. Diagnostics, cancellation notices, rollback failures, and operational failures go to stderr. An error does not retract stdout that was already streamed.

## AutoCAD scheduling and document routing

An AutoCAD process behaves as one serialized main-thread event loop. Documents are contexts within that process, not independent execution loops.

- Each AutoCAD process has one FIFO for all `acadctl` native actions across all of its documents.
- Different AutoCAD processes can execute independently.
- One request targets exactly one document ID.
- Execution temporarily makes the target document current when required, acquires its write lock without prompts, and restores the previously active document afterward.
- No user command, Lisp expression, script, modal operation, or other busy host activity is cancelled to admit an acadctl request.

Admission requires all of the following:

- AutoCAD can service the application-context callback.
- The target document still exists.
- The target document is quiescent.
- The target can become current and be write-locked without prompting.
- AutoCAD can open the required undo group with undo recording enabled.

The pre-execution deadline is five seconds from server acceptance. It includes time behind earlier acadctl jobs and time waiting for AutoCAD or the document to become ready. The deadline is managed off the AutoCAD main thread; the main loop is never put to sleep for polling. Expiry removes the queued job and guarantees that none of its forms started.

The deadline ends when the first form begins. There is no runtime timeout. AutoLISP can legitimately run a long command, display a modal interaction, or wait for user input, and there is no supported safe general cross-thread preemption mechanism.

A conceptual execution state is:

```rust
enum ExecutionState {
    Validating,
    Queued { admission_deadline: Instant },
    Running { next_form: usize },
    RollingBack,
    Succeeded,
    Failed,
    Cancelled,
}
```

This is a state model, not an additional public job API.

## Drawing atomicity

Every accepted `eval` or `exec` containing at least one form opens one undo group in the target drawing. All top-level forms in an `exec` share that group.

- Success closes the group and leaves one natural AutoCAD undo step if the request produced undoable drawing changes.
- A request with no undoable drawing changes does not claim an owned history step.
- AutoLISP failure, native failure, or cooperative cancellation stops execution and rolls the target drawing back to the beginning of the group.
- Rollback is owned by AutoCAD document undo, not `AcTransactionManager`, because arbitrary AutoLISP and nested AutoCAD commands are not contained by a database transaction.

The atomicity boundary is deliberately drawing-only:

- `setq`, `defun`, and other document AutoLISP environment changes are not undone by drawing undo and can remain after a later failure.
- File I/O, COM calls, saves, other drawings, subprocesses, and other external effects are not rolled back.
- Output already sent to stdout remains sent.
- If drawing rollback itself cannot be proved successful, the result is an explicit rollback failure with unknown drawing outcome, and owned history provenance is cleared.

This boundary makes failure behavior honest without pretending arbitrary Lisp side effects are transactional.

## Cancellation, disconnects, and instance termination

### Ctrl+C

Ctrl+C is scoped to the foreground `eval` or `exec` request:

1. The first Ctrl+C sends an explicit cancellation message and keeps the CLI attached while the plugin reaches a safe checkpoint and rolls the drawing back.
2. The second Ctrl+C detaches the CLI immediately. The already-sent cancellation request remains active, and the local process exits with status `130`.

Cancellation is cooperative:

- A queued job is removed before it starts.
- A running `exec` observes cancellation between top-level forms.
- A blocked `acadctl:println` is awakened by cancellation.
- A single running form cannot generally be preempted. If it never returns or reaches a proven native bridge checkpoint, cancellation remains pending.

No execution runtime timeout is layered on top of this model.

### Disconnects

An unexpected client disconnect is not cancellation intent. Once the server accepts a request, the job remains owned by the plugin whether it is queued or running. It continues to success, failure, or its existing cancellation request; further output is discarded.

This choice avoids stopping between forms after non-undoable Lisp or external effects have already happened. A caller that loses the connection without a terminal event has an unknown outcome and must not retry blindly.

### `kill`

`acadctl kill` is the explicit process-level escape hatch:

- If `pid` is omitted, exactly one acadctl-enabled AutoCAD process must exist; otherwise selection fails and the available processes are reported.
- Without `--force`, it requests normal application termination and waits up to five seconds.
- A graceful request that does not finish in five seconds fails without escalation.
- With `--force`, it immediately terminates the OS process even if the plugin and AutoCAD main loop are unresponsive.
- Graceful termination never escalates automatically to forced termination.

Forced termination can lose unsaved work in every document and cannot perform drawing rollback. Ctrl+C never implies either form of `kill`.

## Safe undo and redo

The default history commands operate only on positively known acadctl history. They never probe by issuing `U` or `REDO` against an unknown top entry.

The plugin tracks a conservative, contiguous suffix of owned history per document:

```rust
struct HistoryProvenance {
    undo_suffix: Vec<OwnedStep>,
    redo_suffix: Vec<OwnedStep>,
}

struct OwnedStep {
    execution_id: ExecutionId,
    mode: ExecutionMode,
    source_name: String,
}
```

An unknown barrier is implicit below each suffix. The tracker never claims knowledge of history that existed before observation began.

Rules:

- A successful request is added to `undo_suffix` only when AutoCAD reactor evidence confirms that the request produced the expected undo group.
- A new owned drawing change clears `redo_suffix`.
- A request that produces no undo record, such as a query, `setq`, or `defun`, neither adds nor clears `undo_suffix`.
- A successful safe undo moves the top owned step from `undo_suffix` to `redo_suffix`.
- A successful safe redo moves the top owned step back to `undo_suffix`.
- Repeated safe undo and redo calls may traverse the contiguous owned suffix one step per invocation.
- Any user command or Lisp activity, external database change, manual undo or redo, undo-control change, document replacement, plugin reload, or ambiguous event invalidates knowledge conservatively.
- An intervening Lisp execution invalidates redo knowledge even if it is read-only, because native `REDO` requires immediate history continuity. Known undo ownership can remain when the request provably created no undo record.
- Default redo recognizes only steps previously undone through `acadctl undo`.
- After plugin load or reload, pre-existing undo and redo history is unknown.

If ownership is not positive, the default commands refuse with `nothing safe to undo` or `nothing safe to redo`.

`--force` bypasses provenance and performs exactly one native top undo or redo step. It still fails if no such step exists. A forced history operation clears both owned suffixes because the surrounding history is no longer known. The flag is never added or escalated automatically.

The required evidence comes from execution scope plus editor, document, database, system-variable, undo-boundary, and undo/redo reactor events. C++ forwards the events; Rust owns the provenance state machine. False negatives are acceptable. Acting on a user-owned or unknown step without `--force` is not.

## Execution protocol

`eval` and `exec` share one internal bidirectional `Execute` RPC. The public commands remain distinct; the shared RPC avoids duplicating output, cancellation, admission, and terminal-state machinery.

The first client message is the complete request. `Cancel` is the only valid later client message:

```rust
enum ExecuteClientMessage {
    Request(ExecutionRequest),
    Cancel,
}

struct ExecutionRequest {
    document_id: String,
    mode: ExecutionMode,
    source_name: String,
    source: String,
}

enum ExecutionMode {
    Eval,
    Exec,
}

enum ExecuteServerEvent {
    Accepted,
    Output { text: String },
    Finished(ExecutionOutcome),
}

enum ExecutionOutcome {
    Success { value: Option<String> },
    Failure(ExecutionFailure),
    Cancelled,
}

struct ExecutionFailure {
    message: String,
    form_index: Option<usize>,
    location: Option<SourceLocation>,
    drawing_outcome: DrawingFailureOutcome,
}

struct SourceLocation {
    source_name: String,
    line: usize,
    column: usize,
}

enum DrawingFailureOutcome {
    NotStarted,
    RolledBack,
    Unknown,
}
```

`Accepted` means validation succeeded and the plugin owns the queued job; it does not mean the first form has started. The five-second admission deadline begins at acceptance.

Expected input, busy, AutoLISP, rollback, and cancellation outcomes are terminal execution events so that streamed output can precede them. Non-OK gRPC status is reserved for malformed protocol use, transport failure, or an internal service failure that cannot produce a normal terminal event.

The request contains the whole source in one message. Chunked source upload was rejected because both sides already need the complete batch, the 4 MiB application limit is intentional, and chunking would add protocol states without reducing memory use.

The Rust job queue owns accepted jobs independently of the lifetime of the RPC response sink. This is what allows accidental disconnect to discard output without cancelling work. The stream remains bidirectional only so the foreground CLI can distinguish an explicit first Ctrl+C from connection loss.

There is no IPC protocol version field.

## Host boundaries

### Rust

Rust owns the load-bearing behavior:

- CLI parsing and local input validation.
- UTF-8, BOM, and source-size handling.
- Top-level lexical scanning and source locations.
- Per-instance FIFO admission and deadlines.
- Execution, cancellation, output, and terminal state.
- Bounded output buffering and disconnect behavior.
- Error classification and user-facing formatting.
- Undo/redo provenance.
- RPC messages and service behavior.

### C++

The ObjectARX C++ surface remains bridge boilerplate:

- Register and unregister ObjectARX callbacks and `acadctl:println`.
- Schedule Rust-owned native actions in application context.
- Resolve, activate, lock, and restore document context as directed by Rust.
- Enter the supported synchronous in-memory AutoLISP execution path.
- Open, close, roll back, undo, and redo native undo groups as directed by Rust.
- Forward editor, Lisp, database, document, system-variable, and undo/redo events to Rust.

No queueing policy, scanner, printer policy, error policy, or provenance state belongs in C++.

### AutoLISP shim

A small internal AutoLISP shim may use the reserved `acadctl:` namespace for reader, evaluator, error-capture, and value-formatting support. Only `acadctl:println` is public API. Internal symbol names are implementation details; the tool does not introduce a second `acadctl--` pseudo-namespace.

The evaluator must distinguish an uncaught evaluation error from a successfully returned AutoLISP error object. Returned values remain values; only an error escaping the evaluated form fails the request.

Source remains in memory. A temporary `.lsp` file, `load`, command-line paste, or support-path lookup is not part of the design.

## Rejected alternatives

| Alternative | Why it was rejected |
| --- | --- |
| Interpret `exec` arguments as raw AutoCAD command tokens | The agent-facing language is AutoLISP. Raw tokens lose Lisp composition, definitions, data, and familiar evaluation semantics. |
| Provide only `exec` | Inspection needs a reliable implicit value, while command batches need silence. One command cannot satisfy both without flags or surprising output. |
| Add a separate REPL | Repeated document-scoped invocations already preserve Lisp state, and batch stdin provides multi-form execution without a long-lived interactive protocol. |
| Print every `exec` form value | Assignments and definitions would produce noise, and batch output would be hard to distinguish from intentional output. |
| Print only the last `exec` value | A trailing `setq`, `defun`, or `println` would make the result accidental. That behavior belongs to explicit `eval`. |
| Capture or mirror the AutoCAD command line | It mixes user UI, command echo, prompts, and unrelated output, and does not route cleanly to one client. |
| Replace or wrap standard `princ`, `print`, or `prompt` | Existing AutoLISP must keep its familiar AutoCAD behavior. Agents opt into request output with one explicit function. |
| Name the function `acadctl:tap` | Clojure's `tap>` distributes live values to handlers and interactive tooling; this API emits text. |
| Name the function `acadctl:print` | The chosen behavior always appends a newline and is therefore `println` semantics. |
| Give `println` a label argument or automatic prefix | Variadic concatenation already lets callers write any label, without forcing a formatting protocol. |
| Use Clojure-like `#Type[...]` or AutoCAD's all-caps tags | AutoLISP/Common Lisp-style `#<Type payload>` is more native, and PascalCase is easier to read. |
| Treat every displayed object identity as reusable | AutoCAD does not expose a uniform persistent lookup for selection sets, COM wrappers, files, or functions. Entity handles are the deliberate reusable exception. |
| Use a supported ObjectARX reader/AST API | No such general API is exposed. |
| Build a full AutoLISP parser in Rust | Boundary detection needs only a small lexical state machine. A full parser would duplicate AutoLISP and still not improve runtime source locations enough to justify it. |
| Wrap the entire batch in one synthetic Lisp list or `progn` | The host would lose exact top-level spans and reliable cancellation checkpoints between forms. |
| Execute the whole source through `sendStringToExecute` | It is asynchronous and command-line/echo-oriented, with poor result, error, and undo control. |
| Use `beginExecuteInCommandContext` for admission | It cancels outstanding commands before invoking the callback, violating the rule that acadctl never interrupts user work to begin. |
| Use `ads_queueexpr` at runtime | The API is restricted to drawing-load context and is not a general runtime evaluator. |
| Use `acedInvoke` as a general source evaluator | It invokes registered external functions rather than arbitrary AutoLISP source. |
| Use `AcTransactionManager` as the rollback boundary | Arbitrary AutoLISP and nested commands are not contained by a database transaction. Document undo is the correct drawing-level boundary. |
| Add a runtime timeout | Arbitrary AutoLISP has no safe general forced-interruption point. A timeout would promise cancellation the host cannot reliably deliver. |
| Cancel on disconnect | Connection loss is not intent. Stopping later forms after Lisp or external side effects have occurred can leave a less coherent result than finishing the accepted batch. |
| Add `acadctl cancel` | The foreground request already has Ctrl+C, and there is intentionally no detached job-management surface. |
| Make Ctrl+C equivalent to `kill` | Cancelling one request must not close every drawing in an AutoCAD process. |
| Escalate graceful kill automatically | Forced process termination can lose all unsaved documents and always requires explicit `--force`. |
| Stream source in chunks | The complete source is needed on both sides, 4 MiB is an intentional limit, and chunking adds upload states without a practical benefit. |
| Allow unbounded source | A source batch larger than 4 MiB is almost certainly accidental or should be decomposed. |
| Use exit status `75` for busy | `EX_TEMPFAIL` is established but obscure for an interpreter. Conventional `1`, plus a clear diagnostic, is less surprising. |
| Prefix every error with several categories | The process and source context already identify the tool. A concise two-line diagnostic is easier to read. |
| Build a full parser for inner-expression error columns | The native evaluator does not provide enough correlation to make those locations reliable. The top-level form start is honest. |
| Blindly expose native `U` and `REDO` by default | A remote agent could undo user work. Default history operations require positive ownership. |
| Remove provenance because `redo` exists | Redo can repair only an immediately preceding undo and does not prevent temporarily touching user state. It is a recovery operation, not ownership proof. |
| Persist provenance by modifying the drawing | History tracking must not dirty drawings or add sentinel changes. Conservative in-memory evidence is sufficient and fails closed after reload. |
| Add counts to undo and redo initially | One step per invocation keeps ownership checks and failures precise. Repeated calls traverse a known contiguous suffix. |
| Add IPC protocol versioning | The project is early-stage and does not preserve compatibility with an older execution protocol. |

## Native proof gates

The design depends on behavior that documentation alone does not establish strongly enough on AutoCAD 2027 for Mac. These gates precede the public commands.

| Gate | Required evidence | Consequence if the evidence fails |
| --- | --- | --- |
| Exact document routing | In-memory source evaluates in the requested document's Lisp environment; the prior active document is restored; another document is untouched. | `eval` and `exec` do not ship. |
| Synchronous evaluator | A supported application-context path evaluates one source form synchronously, captures success/error, suppresses wrapper echo, and returns control between forms. | `eval` and `exec` do not ship; asynchronous command-line injection is not substituted. |
| Reader compatibility | Scanner spans agree with AutoLISP for atoms, quotes, strings and escapes, line and block comments, nested lists, dotted data, Unicode, BOM, CRLF, empty input, and incomplete input. | Scanner behavior is corrected before integration. |
| Value capture | Ordinary and opaque values can be formatted without confusing a returned error object with an uncaught error. Entity handles are stable enough for `handent`. | Unsupported values fall back to an honest opaque display; `eval` does not ship if success and failure cannot be distinguished. |
| One undo group | Multiple top-level forms create one natural drawing undo step; read-only or Lisp-only source creates no claimed owned step. | Batch execution does not ship. |
| Rollback | A later form failure and cooperative cancellation restore the target drawing while leaving already documented non-drawing effects outside the guarantee. | Mutating execution does not ship. |
| Busy admission | Active commands, command prompts, scripts, Lisp, dialogs, and unavailable locks are not cancelled or interrupted; expiry happens off the main loop. | Admission behavior is corrected before execution ships. |
| Cancellation checkpoints | A queued request cancels immediately; cancellation is observed between forms; blocked output wakes; one unreturned form remains honestly uninterruptible. | The contract is narrowed to only the checkpoints proven. |
| Output routing | `acadctl:println` reaches only its owning client, preserves order, applies bounded backpressure, becomes a no-op without a sink, and does not alter standard AutoLISP printers. | Public output does not ship until routing is deterministic. |
| Disconnect survival | An accepted job outlives its RPC sink, stops buffering after disconnect, and reaches a terminal internal state without leaking queue entries. | Disconnect semantics are corrected before streaming execution ships. |
| Provenance | Reactor evidence reliably identifies owned undo groups and invalidates on foreign or ambiguous history activity. Safe repeated undo/redo never crosses the unknown barrier. | Default `undo` and `redo` do not ship; blind native history calls are not substituted. |
| Forced history | `--force` performs exactly one native step, reports absence cleanly, and clears provenance. | The flags remain unavailable. |
| Source limit | A 4 MiB source is accepted with protocol overhead, a source one byte larger is rejected by both sides, and no transport default creates a smaller accidental limit. | Transport limits are aligned with the application contract. |
| Process termination | Graceful termination respects the five-second wait and never escalates; forced termination targets only the selected PID. | `kill` remains unavailable until process selection and termination are exact. |

The first implementation artifact is a narrow native vertical slice for routing, synchronous in-memory evaluation, undo grouping, rollback, and reactor provenance. The public CLI and full streaming protocol follow only after those foundations are demonstrated in live AutoCAD. No placeholder `exec`, protocol-version field, or knowingly incomplete history command is added in the meantime.

## Reference behavior

- AutoCAD `UNDO` groups actions between Begin and End as one `U` step: <https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-Core/files/GUID-2729A466-B199-4840-B92B-4D8A38A8ADB8.htm>
- AutoCAD `REDO` reverses one immediately preceding `U` or `UNDO`: <https://help.autodesk.com/cloudhelp/2023/ENU/AutoCAD-Core/files/GUID-BA4CEE11-D8AD-4644-8488-7CEBCA50AFFE.htm>
- AutoLISP comments include `;` line comments and `;| ... |;` block comments: <https://help.autodesk.com/cloudhelp/2024/ENU/AutoCAD-LT-AutoLISP/files/GUID-4D4664AD-301F-4E6E-AD65-4B7CE6A258B8.htm>
- AutoLISP `read` returns the first list or atom from a string: <https://help.autodesk.com/cloudhelp/2023/ENU/AutoCAD-AutoLISP-Reference/files/GUID-5B50BB3E-C244-46E8-85D8-6A2D48B1FE51.htm>
- Clojure's launcher triages read and execution errors around source, location, phase, and cause: <https://clojure.org/reference/repl_and_main#error_printing>

## Implementation decision log

This log is append-only. It records implementation discoveries without rewriting the agreed design above. A later entry may explicitly supersede an earlier implementation choice, but the original entry remains visible.

### 2026-08-13 — I-001: shared Lisp boundary crate

The scanner lives in a small `acadctl-lisp` workspace crate shared by the CLI and plugin. Both sides require identical form boundaries: the CLI for local diagnostics and the plugin for authoritative validation and execution.

Keeping the scanner in the RPC crate was rejected because lexical policy is not transport policy. Duplicating it in the CLI and plugin was rejected because even a small drift could make locally accepted source fail after admission or make the two sides report different form locations.

Scanner byte spans refer to the UTF-8 source after BOM removal. Lines and columns are one-based, columns count Unicode scalar values, and CRLF counts as one line break. An incomplete list, string, block comment, or quoted form points to the opening construct; a stray closing parenthesis points to itself. These choices make diagnostics stable without claiming an inner AutoLISP parser location.

### 2026-08-13 — I-002: one native execution lease per batch

One `eval` or `exec` request enters ObjectARX as one native action and retains the application-context callback, target-document context, write lock, and undo scope across all top-level forms. Rust hands the bridge one owned form step at a time while retaining execution policy and state.

Enqueuing one native action per form was rejected. The existing asynchronous completion path would return to the host between actions, allow unrelated work to interleave, and break the batch's single undo and document-context guarantees.

### 2026-08-13 — I-003: document identity includes the database generation

Execution and history state key a target by both its document token and its current database token. A document object can survive replacement of its underlying database, so the document token alone is insufficient evidence that history still belongs to the same drawing state.

Preserving provenance across a database replacement was rejected. Replacement or an unrecognized token transition clears ownership conservatively, even when this creates a false negative.

### 2026-08-13 — I-004: native calls never retain a Rust state lock

Every Rust-to-C++ execution step is owned data obtained after releasing scheduler state. Lisp evaluation, document changes, and `acadctl:println` can call synchronously back into Rust through reactors or the registered Lisp function.

Holding the queue, execution, output, or provenance mutex across an ObjectARX call was rejected because a reentrant callback could deadlock the AutoCAD main thread. History invalidation events are nevertheless delivered synchronously to Rust so a safe undo decision cannot overtake an earlier user event.

### 2026-08-13 — I-005: process termination is a CLI platform operation

`kill` selects and terminates an operating-system process from the CLI. It does not depend on an RPC or native-plugin action. Graceful termination uses the platform's normal application-termination mechanism and observes the process for five seconds; forced termination targets the selected PID directly.

Putting `kill` behind the plugin was rejected because the command is specifically required to remain useful when the plugin, RPC runtime, or AutoCAD main loop is unresponsive.

### 2026-08-13 — I-006: output capacity is measured in bytes

The execution output path has a total queued-byte budget in addition to bounded message counts. A single `acadctl:println` argument can otherwise make a nominally bounded channel retain an unbounded string.

A bounded channel of unrestricted strings was rejected as insufficient backpressure. Native output is divided into bounded transport chunks while preserving exact concatenated stdout and the one-newline `acadctl:println` contract.

### 2026-08-13 — I-007: form spans are produced incrementally

The shared scanner yields one exact form span at a time and exposes a constant-size resume position. Validation counts forms without retaining every span; execution retains the source, current span, and next scan position rather than a `Vec<FormSpan>` for the whole batch.

This supersedes the conceptual eager `SourceBatch.forms` representation above. A valid 4 MiB source made only of tiny forms can contain more than two million spans, making the eager representation consume roughly 64 MiB before source and transport copies. An additional hidden form-count limit was rejected at this point because incremental scanning satisfies the memory requirement without narrowing the agreed source contract. A form-count admission limit remains possible only if live main-thread measurements justify a separate performance boundary.

### 2026-08-13 — I-008: period boundaries follow the AutoLISP reader

Live AutoCAD 2027 checks established that period is a reader delimiter except inside a complete decimal token that begins with digits. For example, `a.b` begins with form `a`, `1.a` begins with form `1`, `1.2` is one form, `1.` is one form, and leading-dot `.5` is a reader error.

Treating every period as part of an atom was rejected because a per-form evaluator would otherwise read and execute only the prefix while silently leaving an invalid suffix inside the supplied span. Treating every period as a delimiter was also rejected because it would split valid decimals. The scanner therefore recognizes only enough decimal syntax to determine the boundary; AutoLISP remains authoritative for the resulting token's meaning.

### 2026-08-13 — I-009: the boundary scanner remains hand-written

`logos` and `winnow` were evaluated after the first scanner implementation. `logos` provides generated token recognition, spans, callbacks, and lexer extras, but the implementation would still need custom state for balanced lists, quote-plus-trivia attachment, unterminated construct origins, Unicode line and column accounting, AutoLISP's decimal-aware period rule, and an independent resume position. `winnow` provides located and stateful streams with checkpoints, but introduces parser-combinator and checkpoint machinery for a component that intentionally does not parse an AST.

Both dependencies were rejected because neither reduced the net implementation or proof surface for this grammar. The decision can be revisited if the component later becomes a real parser rather than a form-boundary scanner. References: <https://docs.rs/logos/latest/logos/> and <https://docs.rs/winnow/latest/winnow/stream/>.

### 2026-08-13 — I-010: variadic `acadctl:println` is a native registration

Live AutoCAD 2027 testing and Autodesk's `defun` contract established that user-defined AutoLISP functions have fixed arity. `&rest` is not supported and produces a syntax error. The public arbitrary-arity `acadctl:println` is therefore registered through the ObjectARX external-function interface.

C++ performs only mechanical conversion of the `resbuf` argument chain into bounded typed chunks and forwards them synchronously. Rust owns display formatting, opaque-value policy, output routing, byte-budget backpressure, and cancellation wakeups. A Lisp-defined variadic shim was rejected as infeasible; formatting whole values into one Lisp string was rejected because it bypasses the bounded-output contract.

### 2026-08-13 — I-011: mutating native work outlives its RPC handler future

The scheduler, not a tonic handler future, mutex guard, oneshot receiver, response stream, or spawned-task handle, owns every admitted mutating native operation through its terminal state. This applies to document lifecycle, execution, and history actions. Dropping a handler or connection detaches its observer but does not release serialization, abandon rollback, or remove already admitted work.

The earlier lifecycle pattern placed a native action in a global queue and then awaited a oneshot while its handler held the serialization mutex. Dropping that future released the mutex while the queued or already taken native action could still run. Retaining mutation ownership in cancellable futures was rejected because it makes disconnect and server-restart races capable of admitting later work before the earlier outcome is known.

### 2026-08-13 — I-012: cancellation has an atomic finalization boundary

Queued cancellation, five-second admission expiry, and the main-loop claim of a job are resolved by one atomic state transition. After every evaluator return, including the final form, Rust records the evaluator result and checks cancellation before entering finalization. An observed evaluator failure takes precedence over a simultaneous cancellation; cancellation does not erase a real error. A cancellation accepted before finalization rolls back, while one arriving after the atomic transition to finalization is too late and the terminal execution outcome wins.

Rollback retains its initiating cause, and rollback failure always produces failure with an unknown drawing outcome rather than `Cancelled`. This closes the previously unspecified interval after the last `exec` form and prevents cancellation from hiding a drawing state that could not be proved restored.

### 2026-08-14 — I-013: one durable scheduler owns native mutation order

One Rust scheduler owns the document registry, a single FIFO of pending native jobs, and at most one active job for document lifecycle, execution, and history work. A job and its terminal sender enter that scheduler before the RPC handler first suspends. Dropping the handler therefore discards only the reply; it cannot cancel the native mutation or release the serialization lease.

Operation preconditions are evaluated when a job reaches the FIFO head against the latest document and database identity. Even an operation that has become a no-op remains behind earlier admitted work. A readiness wake is coalesced while pending, and C++ takes at most one top-level native action per application-context callback before asking Rust whether another wake is needed. The single `RunExecution` action remains one top-level action even though its native callback later loops over Rust-owned form steps within the same document lease.

The earlier handler-held lifecycle mutex plus a separate native queue was rejected because cancellation of the handler could release one serialization authority while the other still contained its mutation. Draining unrelated actions in one application callback was also rejected because it obscures readiness boundaries and can monopolize the AutoCAD main loop. During plugin shutdown, queued jobs fail explicitly while an already active synchronous job retains ownership until its native completion returns.

### 2026-08-14 — I-014: the native module is not unloadable mid-session

The ObjectARX module remains application-locked after loading. `beginExecuteInApplicationContext` is asynchronous, so a scheduled callback can still hold a raw code address after the scheduling call returns. The registered AutoLISP functions also re-enter the module. Allowing a manual `ARXUNLOAD` without a proven native activity lease could therefore unload code that AutoCAD is about to call.

Mid-session dynamic unload was rejected for the initial implementation. AutoCAD process shutdown still runs the normal plugin teardown, and a later unload feature requires a reference-counted native callback lease whose last release is proven to occur after every queued application callback and nested AutoLISP callback has returned. Plugin reload testing consequently uses a fresh AutoCAD process rather than weakening callback lifetime safety.

### 2026-08-14 — I-015: one-form evaluation uses staged symbols and a fixed driver

The private execution bridge stages an exact scanner span in the target document's AutoLISP environment through a reserved `acadctl:*source*` symbol, then invokes one compile-time embedded, fixed AutoLISP expression synchronously. The driver wraps the staged text in an outer list, requires that the reader produce exactly one element, evaluates that element, and writes only tagged status, error, `ERRNO`, and value state to reserved symbols. Request source is never interpolated into a command string, written to a temporary file, or resolved through AutoCAD support paths.

The success tag is constructed inside `vl-catch-all-apply`. Live AutoCAD 2027 checks proved that a form which successfully returns a catch-all error object remains a successful value, while an error escaping `read` or `eval` remains a request failure. Classifying the returned payload itself with `vl-catch-all-error-p` was rejected because it cannot distinguish those cases.

Successful `exec` form values are cleared immediately. The eventual `eval` path will retain its one value through successful undo-group finalization, stream its readable representation only afterward, and then clear it. Serializing or emitting the implicit value before `_UNDO _End` was rejected because a later finalization failure must produce no implicit value, and buffering a whole rendered value would violate bounded output.

### 2026-08-14 — I-016: U+0000 is rejected before native staging

Source containing U+0000 is invalid at the CLI boundary and at the authoritative plugin boundary. ObjectARX stages strings through a NUL-terminated native interface, so accepting an interior zero byte would allow the Rust scanner and the target AutoLISP reader to observe different source. Live AutoCAD 2027 checks also established that ordinary AutoLISP string constructors discard character code zero, so this rejection does not exclude a representable AutoLISP source character.

Silently truncating at U+0000 was rejected because the displayed and validated batch could differ from the executed batch. Treating the CLI check alone as sufficient was rejected because another local RPC client could bypass it.

### 2026-08-14 — I-017: rollback failure remains an execution outcome

Rust owns the structured terminal outcome even when native rollback fails. After C++ reports a failed rollback step, the execution state records the original failure plus the rollback failure and marks the drawing outcome unknown. The native step loop may then finish with an unknown undo-group state without replacing that result with a generic bridge-protocol error.

Issuing an additional blind `U` after a successfully closed group was rejected as internal-error recovery. A group that created no undo record could expose the preceding user-owned history step. Internal result-correlation failure therefore reports an unknown bridge outcome; it attempts rollback only while the bridge still positively knows that its own group is open. Normal rollback itself remains behind the live undo proof gate.

### 2026-08-14 — I-018: native execution is compile-proved but not runtime-proved

The private bridge compiles and links against the installed ObjectARX 2027 SDK with the required application-context, document-locking, symbol, synchronous-command, and undo APIs. The embedded reader/evaluator expression has separately passed live AutoLISP conformance checks in AutoCAD 2027.

These are not yet evidence that `acedPutSym` followed by `acedCommandS` works through the plugin callback, that an inactive target receives the correct Lisp environment without UI activation, or that `_UNDO _Begin`, `_End`, and `U` provide the expected reactor-observable group. Replacing the installed trusted ApplicationAddins bundle with the newly built private slice was not performed because that persistent external change requires explicit user approval. The public commands remain absent, and static linkability plus a manually entered Lisp oracle are not reported as a passed native proof gate.

### 2026-08-14 — I-019: execution starts only from a stable current-document context

The application-context callback admits execution only when AutoCAD's current document and MDI-active document are the same stable, quiescent document and `CMDACTIVE` reports no command, script, Lisp, or dialog activity. It then locks the target, makes the target current without activation, and verifies the exact current pointer, unchanged active pointer, document token, and database token before opening the undo group. Cleanup restores current to the previously active document, following the ObjectARX document-manager contract, and verifies both current and active pointers before unlocking.

Restoring an arbitrary pre-callback current pointer was rejected. ObjectARX explicitly permits current and MDI-active to differ temporarily and instructs callers to reset current to the active document after temporary database work. Admitting work while those pointers already differ was also rejected because acadctl cannot prove that it is not intruding on another native context transition.

The expected active pointer and database token remain attached to the whole native lease. Before and after every native execution step, the bridge rechecks the exact target current pointer, unchanged active pointer, and database token before reading another implicit system variable or issuing another command. If a form loses that context, Rust terminalizes the request with an unknown drawing outcome and C++ never attempts rollback through the wrong document context. If an owned undo group may remain in the displaced context, the scheduler enters the quarantine described below.

### 2026-08-14 — I-020: an unproved native lease quarantines mutation scheduling

Restore failure, an unexpected current or active document, unlock failure, an undo group that cannot be proved closed, and database replacement while a group may be open all make the process's native mutation context unknown. The current request retains its structured failure and unknown drawing outcome, but the one-process mutation scheduler is then quarantined: queued mutations fail, new mutations are rejected, stale callbacks take no action, and no next application-context callback is scheduled. Read-only document listing remains available. Only a fresh AutoCAD process clears this condition.

Releasing the active scheduler job and continuing after such a failure was rejected. A leaked document lock, wrong current context, or open undo group would invalidate FIFO isolation for every later lifecycle, execution, or history action. Treating a server reconnect as recovery was rejected because restarting the RPC runtime does not repair AutoCAD native state.

### 2026-08-14 — I-021: undo command status and observed group state are separate evidence

Before `_UNDO _Begin`, the bridge reads `UNDOCTL` and refuses admission when bit 8 shows an existing group. Every Begin and End command returns both its command result and the subsequently observed group state. Rust receives the operation result; the mechanical native lease separately remembers whether the owned group is active, inactive, or unknown. A failed End with the group proved inactive permits later scheduling after the drawing outcome is reported unknown; an End whose closure cannot be proved quarantines the scheduler.

Using `undoRecording()` alone was rejected because it says recording is enabled but does not exclude a pre-existing user group. Treating `acedCommandS` success or failure as proof of the resulting group state was rejected because a command can partially transition before reporting failure. Issuing `U` when this request never positively opened a group was rejected because it could undo the preceding user-owned step.

The normal rollback path remains a live proof gate. In addition to proving `_End` plus `U` restores representative failures, verification must determine whether that sequence leaves the failed request on AutoCAD's redo stack. A rollback that can later be resurrected by an ordinary user `REDO` is not accepted as the final implementation; static command return codes do not answer that question.

### 2026-08-14 — I-022: native staging roots and transient conversions are explicitly released

The Rust execution owns one `Arc<String>` source and hands C++ borrowed form slices. The temporary macOS `AcString` form conversion is scoped only through `acedPutSym` and is destroyed before the evaluator runs, avoiding roughly 16 MiB of avoidable overlap for a 4 MiB ASCII form. The fixed evaluator conversion is constructed once per batch rather than once per form.

Every reserved Lisp staging symbol is cleared before and after evaluation, and a failed clear is a native form failure rather than a reported success. Ignoring cleanup was rejected because `acadctl:*source*` or an arbitrarily large `acadctl:*value*` could remain globally rooted and contaminate later requests. Terminal outcomes and error details are moved out of the scheduler job instead of cloned when ownership can be transferred.

### 2026-08-14 — I-023: output state is independent and fragment-coalescing

Each accepted execution owns a separate output state shared by one synchronous producer and one asynchronous consumer. The producer blocks only on that output state's byte-budget condition variable; it never retains the scheduler, execution-state, or native-event mutex. The consumer removes a chunk only when its `next_chunk` future returns, so dropping a pending read cannot consume output. Cancellation, sink disconnect, and plugin stop are independent latches and all wake a blocked producer.

The initial infrastructure budget is 256 KiB of queued UTF-8 divided into chunks no larger than 16 KiB. Adjacent fragments are coalesced into the current chunk, including across renderer calls, so many one-byte values cannot turn the byte budget into unbounded per-fragment allocation metadata. Chunks split only at UTF-8 boundaries and concatenating them reproduces stdout byte for byte. Disconnect clears queued data and makes later emission a no-op without clearing an already latched cancellation; cancellation stops later emission but retains bytes already queued for the still-connected client.

A Tokio channel bounded only by message count was rejected because each message could contain an unrestricted string. Holding the scheduler lock while waiting for space was rejected because `acadctl:println` re-enters Rust synchronously on AutoCAD's main thread. Making async read ownership part of the queue entry was rejected because cancelling that future could lose an output chunk.

### 2026-08-14 — I-024: output fragments become readable at bounded flush points

Renderer fragments remain private to the producer until their chunk reaches 16 KiB or the completed `acadctl:println` or implicit eval value explicitly flushes it. Completed small lines are merged into the last unread transport chunk when capacity permits. A connected client therefore receives a completed line promptly, while a fast consumer cannot turn every string escape, list delimiter, or one-byte argument into a separate allocation and RPC event. Large values still expose full chunks incrementally and apply the same byte-budget backpressure before the value finishes.

Notifying the consumer after every fragment was rejected after a clean-context performance audit demonstrated a schedule in which the consumer removed each one-byte partial tail before the next fragment arrived. Coalescing only while the consumer happened to lag did not establish the bounded-message property recorded in I-006 and I-023.

### 2026-08-14 — I-025: output conditions are durable predicates and transport is single-flight

Cancellation, disconnect, stop, completion, and available byte capacity are durable state predicates checked while holding the output mutex immediately before every condition-variable wait. Notifications only prompt another predicate check; they are never the evidence for a transition. Publishing a partial chunk does not introduce an unlock-and-relock interval before that check. This prevents cancellation or completion from being signalled just before a producer begins waiting and then being lost.

The eventual RPC writer owns at most one removed 16 KiB chunk and awaits its transport send before reading the next. It does not feed a second task or queue. The infrastructure bound is therefore the 256 KiB shared queue plus one bounded in-flight chunk. Releasing queue accounting and then collecting several returned chunks was rejected because it would move unbounded retention outside the measured budget. The pending buffer grows only with actual content rather than reserving 16 KiB for every small flushed line; this avoids fixed-size allocation churn while preserving the same payload and chunk bounds.

### 2026-08-14 — I-026: output limits account payload rather than allocator capacity

The 256 KiB shared limit and 16 KiB single-flight limit bound live UTF-8 payload, not the allocator's exact retained bytes. Each nonempty string has at most one chunk of content and the number of ready and pending strings is bounded, but an allocator may reserve more capacity than the current length. The implementation and memory tests therefore report payload, chunk-count, and single-flight bounds separately from measured resident memory.

Describing the payload limit as an allocator-exact heap limit was rejected after the memory audit showed that appending to an odd-sized `String` can grow its capacity beyond its final 16 KiB length. This remains bounded infrastructure overhead rather than an unbounded retention path, and live memory measurements remain a release gate for the native/RPC integration.

### 2026-08-14 — I-027: Rust renders typed value events

Rust's value printer consumes an iterative stream of structural and typed events: list begin and end, dotted-tail markers, strings in bounded chunks, symbols, numbers, points, and normalized opaque kinds. It owns list spacing, display versus readable string behavior, stable PascalCase opaque tags, payload sanitization, the final newline, and the explicit output flush. C++ only classifies documented `resbuf` fields and forwards bounded text; a private fixed-arity Lisp visitor may normalize symbols and unsupported opaque values before crossing the same event boundary.

Live AutoCAD 2027 checks established that `prin1` and `princ` differ recursively for strings, while lists use conventional proper and dotted syntax. Readable strings escape quote, backslash, line feed, carriage return, and tab; literal Unicode remains literal. Stable acadctl opaque displays deliberately replace AutoCAD's pointer-bearing entity and function printers and path-bearing file printer. Whole-composite `vl-prin1-to-string`, native command-line capture, and C++ formatting policy remain rejected because they either allocate without the output budget, mix console output, or move load-bearing behavior into the bridge.

Documented `resbuf` types do not cover every promised AutoLISP opaque family. Public arbitrary-value `acadctl:println`, nested opaque normalization, and the unavoidable AutoCAD-side allocation for a huge variadic argument graph therefore remain native live proof gates. The formatter being total for its typed event vocabulary is not treated as proof that AutoCAD can marshal every source value into that vocabulary.

### 2026-08-14 — I-028: native numeric text and structural limits preserve Lisp semantics safely

Real-number events carry a bounded, validated AutoLISP-normalized token rather than a raw binary float for Rust to format. Live AutoCAD 2027 checks showed identical `prin1` and `princ` results independent of `LUPREC`: `1.0`, `0.0` for negative zero, `1.23457`, `1.0e-12`, and `1.0e+20`. Rust's shortest-decimal formatting was rejected as materially different. Non-finite or syntactically non-real tokens are rejected at the event boundary. Integer text remains exact decimal, and point coordinates use the same normalized real tokens.

String and symbol atoms cross in UTF-8 fragments no larger than 16 KiB. Readable strings additionally implement AutoLISP's `\e` escape and three-digit octal control escapes, not only quote, backslash, newline, return, and tab. Opaque entry points are kind-specific: entity handles are validated and uppercased, selection sets use descriptive numbers, class and function labels are bounded and reject pointer-like text, and files and errors accept no payload. This prevents the bridge from reintroducing native addresses or file paths through an arbitrary opaque string.

The iterative list-state stack is one byte per open list and is limited to 65,536 levels. Deeper structure is represented honestly as `#<Object DepthLimit>` while its nested events are skipped with constant additional state. This bounds renderer-owned nesting metadata without recursion. A native or Lisp visitor must separately detect cycles or enforce a finite traversal-step policy; the typed stream alone has no source-object identity and cannot prove that a cyclic producer will terminate. An unbounded renderer stack and treating output backpressure as a bound on structural metadata were rejected.

Cancellation publishes already formatted bytes, prevents later emission, and ends the output stream after those bytes drain. The execution outcome remains independently scheduler-owned, so a hung form may keep the request alive after its output substream has ended. Waiting for another output notification after cancellation was rejected because no producer can legally add more bytes once the cancellation latch is set.

### 2026-08-14 — I-029: zero-output value events still observe terminal state

Every typed value event checks the output sink even when it produces no visible bytes, including empty fragments and events inside the depth-limit fallback. Cancellation, disconnect, stop, and completion can therefore unwind a native traversal that is currently suppressing output instead of relying on a future printable event. The native visitor still owns a finite traversal-step or cycle policy because output polling cannot make a nonterminating producer structurally safe by itself.

An empty symbol sequence is rejected rather than silently consuming a list position; list spacing is deferred until the first nonempty symbol fragment. Normalized real tokens require digits on both sides of the decimal point and a complete optional exponent, so `.5`, `1.`, and incomplete exponents cannot cross the typed boundary. Trusting Rust's more permissive floating-point parser as an AutoLISP grammar oracle was rejected.

Readable control escapes are assembled from a fixed four-byte octal buffer. Formatting each control character through a temporary heap string was rejected after performance review showed that a single bounded 16 KiB atom could otherwise cause more than sixteen thousand tiny allocations.

### 2026-08-14 — I-030: raw symbol names are the typed identity boundary

Symbol events carry one or more bounded raw UTF-8 name fragments obtained from AutoLISP's symbol value, not rendered printer text. Live AutoCAD 2027 checks established that reader-created symbols cannot be empty or contain whitespace, parentheses, quote, semicolon, apostrophe, or period delimiters. Vertical bars are ordinary name characters rather than Common Lisp quoting syntax. Rust rejects delimiter-bearing fragments so a malformed native event cannot inject list structure into the output stream.

Display mode emits the raw name. Readable mode duplicates every backslash, matching AutoLISP's observed `prin1` behavior, but adds no quotes or vertical-bar wrapper. AutoLISP itself does not read that doubled form back to the original backslash-bearing symbol, so rendered output is deliberately not treated as symbol identity or a general serialization format. Claiming readable value output is universally reader-round-trippable was rejected; the raw typed name remains the lossless internal boundary.

### 2026-08-14 — I-031: symbol validity is checked with constant streaming state

The symbol printer carries a small lexical state across chunks and rejects an empty name, exact case-insensitive `NIL`, a complete signed or unsigned integer, and a complete scientific real. Live AutoCAD 2027 checks established that `123`, `+123`, and `-123` are integers; `1e3` and `1e-3` are reals; and `+`, `-`, `123A`, `1E`, and incomplete exponents remain symbols. This distinction cannot be recovered from per-fragment character checks alone.

Buffering an entire symbol before output was rejected because symbol names have no agreed total-size limit. The formatter instead retains only a constant-size number state and `NIL` matcher. The native producer must emit `Symbol` only for an actual AutoLISP symbol and `nil` through its distinct event; the Rust checks fail closed if that invariant is violated. A malformed internal event can be diagnosed after earlier chunks were streamed, but it cannot arise from a correctly typed value. Treating defensive validation as a reason to duplicate an arbitrarily long symbol in memory was rejected.

### 2026-08-14 — I-032: execution mode is an admission invariant

Rust records `Eval` or `Exec` on the execution before it enters the native scheduler. `Eval` requires exactly one top-level form after BOM removal and complete source validation. `Exec` accepts zero or more forms and preserves their source order. Both modes use the same scanner and native execution lease, while the stored mode later controls implicit-value output and history presentation.

Inferring the mode from output behavior or deferring the one-form check to the native AutoLISP wrapper was rejected. The authoritative plugin boundary must reject an invalid `eval` batch before scheduling any document work, and later stages must not reconstruct caller intent from incidental state. Giving `eval` and `exec` separate execution engines was also rejected because their routing, undo, rollback, cancellation, and error semantics are otherwise identical.
