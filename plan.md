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
