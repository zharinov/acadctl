# `acadctl eval`, `exec`, and drawing history

Status: the installed private build runs document-scoped `eval`, `exec`, one-value `acadctl:println`, rollback, drawing-wide undo and redo, and exact process termination on macOS and AutoCAD 2027. The implementation and its post-commit first-principles naming pass are committed locally; cleanup and independent spec audits remain. Windows-specific runtime validation remains a platform porting gate; its CLI path cross-compiles but was not executed on this Mac.

## Current implementation checklist

- [x] Bounded scanner, Rust execution state machine, output/value rendering, RPC, and CLI.
- [x] Live document-scoped `exec`, `eval`, `acadctl:println`, readable values, Lisp diagnostics, and drawing rollback.
- [x] Live drawing-wide `undo` and `redo`, including exact inactive-document routing and restoration of the prior active document.
- [x] Live cancellation, disconnect, blocked-output, busy-admission, maximum-source, and process-lifecycle gates on macOS and AutoCAD 2027.
- [x] Commit the one-value output and process-identity corrections, then complete the first-principles naming reconciliation.
- [ ] Complete the cleanup and independent spec audits.

## Design center

`acadctl` is a document-aware AutoLISP runner for agents. It is not a second AutoCAD command line and does not introduce a new Lisp dialect.

The design has five priorities:

1. Familiar AutoLISP behavior for source, values, definitions, and printing.
2. Exact routing to one open document without disturbing work already happening in AutoCAD.
3. One drawing undo group per non-empty request, with drawing rollback on failure or cancellation.
4. Predictable Unix behavior for stdin, files, stdout, stderr, exit status, Ctrl+C, and broken connections.
5. A minimal C++ bridge, with queueing, scanning, state, protocol, formatting, and policy owned by Rust.

AutoLISP remains fully capable code. `acadctl` is not a sandbox. Agent instructions remain responsible for avoiding document lifecycle operations, explicit undo manipulation, saves, cross-document COM changes, and other effects that do not belong in a request.

## Public command line

| Command | Meaning | Successful stdout |
| --- | --- | --- |
| `acadctl eval <id> [file]` | Evaluate exactly one top-level AutoLISP form. | The form's readable value, followed by a newline; explicit `acadctl:println` output comes first. |
| `acadctl exec <id> [file]` | Execute zero or more top-level AutoLISP forms as one batch. | Only explicit `acadctl:println` output. |
| `acadctl undo <id>` | Undo the drawing's last AutoCAD history step. | Nothing. |
| `acadctl redo <id>` | Redo the drawing's next AutoCAD history step. | Nothing. |
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
| `(acadctl:println (strcat "created: " (itoa count)))` | `created: 12`, then `nil` | `created: 12` |

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
(acadctl:println (strcat "created: " (itoa count)))
```

Its contract is:

- Exactly one argument.
- Ordinary values use familiar `princ`-style display semantics; strings are not quoted.
- Opaque values use the stable acadctl display forms described below.
- Exactly one newline is added after the value.
- The CLI forwards each received line immediately and flushes stdout; buffering does not wait for request completion.
- Output is routed only to the client owning the active `eval` or `exec` request.
- The function returns `nil`.
- Outside an active request, it has no effect and still returns `nil`.
- After a client disconnects, it has no visible effect and returns `nil` while the accepted job continues.

Standard AutoLISP `princ`, `prin1`, `print`, `prompt`, and related functions are not replaced or captured. They retain their normal AutoCAD command-line behavior. This preserves existing Lisp expectations and keeps the user's AutoCAD console separate from the request's stdout.

No automatic label, prefix, or request ID is added to output. Callers can construct one string when they want a label beside another value.

### Value printers

There are two related printer modes:

- The implicit `eval` result uses readable Lisp-native formatting for ordinary values, analogous to `prin1`.
- `acadctl:println` uses display formatting for ordinary values, analogous to `princ`.

Both modes use stable forms for opaque values, including when an opaque value is nested inside ordinary data:

```text
#<Entity 5A2>
#<SelectionSet>
#<VlaObject>
#<File>
#<Function>
```

Type names use PascalCase. A payload appears only when it is useful. These forms are displays, not new readable AutoLISP literals.

Only an entity handle is intentionally reusable identity. The entity from `#<Entity 5A2>` can later be resolved with `(handent "5A2")`, subject to the entity still being live in that drawing. Selection-set, VLA-object, file, and function displays are descriptive type tags rather than general object handles.

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

- Each AutoCAD process has one Rust-owned FIFO for all `acadctl` mutation jobs across all of its documents.
- Different AutoCAD processes can execute independently.
- One request targets exactly one document ID.
- Execution queues one fixed AutoLISP driver in the target document, activates that document when required, and restores the previously active document afterward.
- No user command, Lisp expression, script, modal operation, or other busy host activity is cancelled to admit an acadctl request.

Admission requires all of the following:

- AutoCAD can service the application-context dispatch callback.
- The target document still exists.
- The target document is quiescent.
- The target can become active and accept the fixed document-context driver without prompting.
- AutoCAD can open the required undo group with undo recording enabled.

The execution-start deadline is five seconds from server acceptance. It includes time behind earlier acadctl jobs and time waiting for AutoCAD or the document to become ready. The deadline is managed off the AutoCAD main thread; the main loop is never put to sleep for polling. Expiry removes the queued job and guarantees that none of its forms were handed off.

The deadline ends when Rust hands off the first `EvaluateForm` step. There is no runtime timeout. AutoLISP can legitimately run a long command, display a modal interaction, or wait for user input, and there is no supported safe general cross-thread preemption mechanism.

A conceptual execution state is:

```rust
enum ExecutionState {
    Validating,
    Queued { execution_start_deadline: Instant },
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

- Success closes the group and leaves one natural AutoCAD undo step.
- Empty `exec` is the only execution that skips the group entirely. AutoCAD may create a group record for non-empty read-only or Lisp-only work; acadctl does not infer otherwise.
- AutoLISP failure, native failure, or cooperative cancellation stops execution and rolls the target drawing back to the beginning of the group.
- Rollback is owned by AutoCAD document undo, not `AcTransactionManager`, because arbitrary AutoLISP and nested AutoCAD commands are not contained by a database transaction.

The atomicity boundary is deliberately drawing-only:

- `setq`, `defun`, and other document AutoLISP environment changes are not undone by drawing undo and can remain after a later failure.
- File I/O, COM calls, saves, other drawings, subprocesses, and other external effects are not rolled back.
- Output already sent to stdout remains sent.
- If drawing rollback itself cannot be proved successful, the result is an explicit rollback failure with unknown drawing outcome.
- Successful rollback describes the drawing state when the request finishes. AutoCAD can retain the rolled-back group as the next redo step; a later ordinary `acadctl redo` or interactive `REDO` may therefore reapply it.

This boundary makes failure behavior honest without pretending arbitrary Lisp side effects are transactional.

## Cancellation, disconnects, and instance termination

### Ctrl+C

Ctrl+C is scoped to the foreground `eval` or `exec` request:

1. The first Ctrl+C sends an explicit cancellation message and keeps the CLI attached while the plugin reaches a safe checkpoint and rolls the drawing back.
2. The second Ctrl+C requests detachment. The CLI exits after the plugin acknowledges the cancellation or reports that it is too late.
3. A third Ctrl+C is the explicit unconfirmed escape. It exits with status `130` and warns that the accepted job may still be running.

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

- If `pid` is omitted, exactly one AutoCAD process must exist; otherwise selection fails and the available processes are reported. Plugin availability is irrelevant.
- Without `--force`, it requests normal application termination and waits up to five seconds.
- A graceful request that does not finish in five seconds fails without escalation.
- With `--force`, it immediately terminates the OS process even if the plugin and AutoCAD main loop are unresponsive.
- Graceful termination never escalates automatically to forced termination.

Forced termination can lose unsaved work in every document and cannot perform drawing rollback. Ctrl+C never implies either form of `kill`.

## Undo, redo, and save

`undo` and `redo` deliberately use the same drawing-wide history the user sees inside AutoCAD. They do not distinguish user changes from acadctl changes, do not maintain provenance, and do not inspect or infer ownership.

Each invocation:

- resolves the document ID to one exact open document/database generation at the FIFO head;
- requires a quiescent document and fails busy without cancelling user work;
- establishes and later restores the native document context;
- issues exactly one fixed native `U` or `REDO` command;
- returns the refreshed document state and prints nothing on success.

There is no `--force`, count, unknown barrier, safe-history mode, reactor trace grammar, or hidden repair command. Repeated invocations provide repeated traversal. If there is no applicable native history step, acadctl reports the native command result without probing through an extra mutation.

`save` is the persistence checkpoint. Users who want a durable recovery point save before handing control to an agent, exactly as they do before risky interactive work. Saving does not clear or partition AutoCAD's undo history: subsequent undo or redo can still change the in-memory drawing, and another save is required to persist that later state.

## Execution protocol

`eval` and `exec` share one internal bidirectional `Execute` RPC. The public commands remain distinct; the shared RPC avoids duplicating output, cancellation, admission, and terminal-state machinery.

The first client message is the complete request. `Cancel` is the only valid later client message:

```rust
enum ExecutionClientMessage {
    Request(ExecutionRequest),
    Cancel(ExecutionCancelRequest),
}

struct ExecutionRequest {
    document_id: String,
    mode: ExecutionMode,
    source_name: String,
    source: Bytes,
}

enum ExecutionMode {
    Eval,
    Exec,
}

enum ExecutionServerEvent {
    Accepted,
    Output { chunk: String },
    CancelAcknowledgement(ExecutionCancelAcknowledgement),
    Finished(ExecutionOutcome),
}

enum ExecutionOutcome {
    Success,
    Failure(ExecutionFailure),
    Cancelled,
}

struct ExecutionCancelAcknowledgement {
    disposition: ExecutionCancelDisposition,
}

enum ExecutionCancelDisposition {
    Accepted,
    TooLate,
}

struct ExecutionFailure {
    message: String,
    form_index: Option<usize>,
    location: Option<SourceLocation>,
    drawing_outcome: DrawingOutcome,
}

struct SourceLocation {
    source_name: String,
    line: usize,
    column: usize,
}

enum DrawingOutcome {
    NotStarted,
    RolledBack,
    Committed,
    Unknown,
}
```

`Accepted` means validation succeeded and the plugin owns the queued job; it does not mean the first form has started. The five-second execution-start deadline begins at acceptance.

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
- Drawing-wide undo/redo direction, FIFO placement, and exact document-generation targeting.
- Bounded output buffering and disconnect behavior.
- Error classification and user-facing formatting.
- RPC messages and service behavior.

### C++

The ObjectARX C++ surface remains bridge boilerplate:

- Register and unregister lifecycle callbacks and the private Lisp bridge callbacks.
- Schedule Rust-selected native work in application context.
- Resolve, establish, and restore document context as directed by Rust; lock only native database work that requires an explicit application-context lock.
- Enter the target document's AutoLISP driver and exchange one staged form or value event per registered callback.
- Open, close, and roll back the current execution's native undo group as directed by Rust.
- Issue one fixed `U` or `REDO` action selected by Rust.
- Coalesce native database changes only to refresh the Rust-owned document snapshot.

No queueing policy, scanner, printer policy, error policy, history model, or document state belongs in C++.

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
| Give `println` a label argument or automatic prefix | Callers can construct the one value they want to display, normally with `strcat`, without forcing a formatting protocol. |
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
| Track whether a history step belongs to acadctl | AutoCAD exposes drawing-wide undo/redo, not a stable supported top-record identity. Ownership inference is version-sensitive complexity that disagrees with the user's normal history model. |
| Add `--force` or a separate safe-history mode | There is only one drawing-wide meaning. Saving is the explicit persistence checkpoint; history commands do not pretend to add a stronger safety boundary. |
| Add counts to undo and redo initially | One fixed native step per invocation keeps routing, failures, and user intent precise. Repeated calls provide repeated traversal. |
| Add IPC protocol versioning | The project is early-stage and does not preserve compatibility with an older execution protocol. |

## Native proof gates

The design depends on behavior that documentation alone does not establish strongly enough on AutoCAD 2027 for Mac. These gates precede the public commands.

| Gate | Required evidence | Consequence if the evidence fails |
| --- | --- | --- |
| Exact document routing | In-memory source evaluates in the requested document's Lisp environment; the prior active document is restored; another document is untouched. | `eval` and `exec` do not ship. |
| Document-context evaluator | A supported target-document Lisp driver evaluates one staged source form at a time, captures success/error, suppresses wrapper echo, and returns control to Rust between forms. | `eval` and `exec` do not ship until the driver and callback lifecycle are exact. |
| Reader compatibility | Scanner spans agree with AutoLISP for atoms, quotes, strings and escapes, line and block comments, nested lists, dotted data, Unicode, BOM, CRLF, empty input, and incomplete input. | Scanner behavior is corrected before integration. |
| Value capture | Ordinary and opaque values can be formatted without confusing a returned error object with an uncaught error. Entity handles are stable enough for `handent`. | Unsupported values fall back to an honest opaque display; `eval` does not ship if success and failure cannot be distinguished. |
| One undo group | Multiple top-level forms create one natural drawing undo step. Empty `exec` creates none; AutoCAD's behavior for other non-mutating groups is accepted as native history behavior. | Batch execution does not ship. |
| Rollback | A later form failure and cooperative cancellation restore the target drawing while leaving already documented non-drawing effects outside the guarantee. Immediate `REDO` may reapply the rolled-back group. | Mutating execution does not ship. |
| Busy admission | Active commands, command prompts, scripts, Lisp, dialogs, and unavailable locks are not cancelled or interrupted; expiry happens off the main loop. | Admission behavior is corrected before execution ships. |
| Cancellation checkpoints | A queued request cancels immediately; cancellation is observed between forms; blocked output wakes; one unreturned form remains honestly uninterruptible. | The contract is narrowed to only the checkpoints proven. |
| Output routing | `acadctl:println` reaches only its owning client, preserves order, applies bounded backpressure, becomes a no-op without a sink, and does not alter standard AutoLISP printers. | Public output does not ship until routing is deterministic. |
| Disconnect survival | An accepted job outlives its RPC sink, stops buffering after disconnect, and reaches a terminal internal state without leaking queue entries. | Disconnect semantics are corrected before streaming execution ships. |
| Drawing history | One fixed `U` or `REDO` executes in the exact requested document, affects the same history visible to the user, reports native absence/failure honestly, and restores the prior current/active context. | `undo` and `redo` do not ship until routing and context restoration are exact. |
| Source limit | A 4 MiB source is accepted with protocol overhead, a source one byte larger is rejected by both sides, and no transport default creates a smaller accidental limit. | Transport limits are aligned with the application contract. |
| Process termination | Graceful termination respects the five-second wait and never escalates; forced termination acts only after the retained platform process identity still resolves to the same AutoCAD instance. | `kill` remains unavailable until process selection and termination are exact. |

The implementation remains a narrow native bridge for routing, fixed AutoCAD operations, evaluation, undo grouping, rollback, and lifecycle observation. Rust owns scanning, queueing, state, protocol, formatting, and policy. No protocol-version field or native history state machine is introduced.

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

This historical checkpoint is superseded by I-069.

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

I-068 supersedes the earlier requirement that rollback remove its redo entry. The bridge must still prove that `_End` plus `U` restores the drawing when the request finishes. A later ordinary `REDO` may reapply that group as part of AutoCAD's drawing-wide history.

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

### 2026-08-14 — I-033: execution owns output independently of its waiter

Every validated execution is created with one bounded producer sink and one asynchronous output stream. The scheduler-owned execution retains the producer after admission, so dropping the future awaiting its terminal result cannot close output, release the native lease, or admit a later mutation. Conversely, dropping an execution before its admission drops the last producer and stops the otherwise unterminated stream. A manually cloned producer keeps the stream alive until the last clone reaches a durable terminal state.

Normal native completion publishes pending bytes and finishes the output stream before sending the terminal execution result. Cancellation publishes pending bytes and ends the stream after they drain. Plugin shutdown, wake failure, and rejection of queued work stop their streams and wake blocked producers. The scheduler clones the relevant sink while holding its state lock, then performs `finish`, `request_cancel`, or `stop` after releasing that lock; output backpressure is never part of the scheduler critical section.

Making the RPC response future own the producer was rejected because Rust Future cancellation would then close output or release execution state after the plugin had accepted the mutation. Leaving a stream pending when every producer disappears was also rejected because an unpolled or abandoned pre-admission future could otherwise strand its output task indefinitely.

### 2026-08-14 — I-034: cancellation distinguishes empty-group close from drawing rollback

Queued cancellation and main-loop claim are serialized by the scheduler mutex: either cancellation removes the queued execution and completes it as `Cancelled`, or the native callback owns it. An active cancellation accepted before commit handoff latches request state under the same mutex, then wakes bounded output after releasing the mutex. Repeated cancellation remains accepted while the active job is scheduler-owned; the later Execute stream latches its first accepted Cancel so a duplicate after queued removal does not require scheduler tombstones. Handing the `Commit` step to the native loop is the finalization boundary; cancellation after that point is too late and does not suppress post-commit output.

Cancellation before `_UNDO _Begin` completes without opening a group. If cancellation wins while Begin is already executing and Begin succeeds, Rust yields an explicit `Abort` step that issues only `_UNDO _End`. Once any form has been attempted, cancellation yields the normal rollback step, `_UNDO _End` followed by `U`. This distinction prevents an empty cancelled group from exposing and undoing the preceding user-owned history entry. C++ rejects an `Abort` after a form was attempted and retains its emergency rollback path rather than committing through an inconsistent Rust state.

An evaluator failure observed after cancellation still becomes the terminal failure and keeps its source location. A failed cancellation rollback, failed empty-group close, lost native context, or failed lease cleanup overrides `Cancelled` with failure and an unknown drawing outcome. Plugin shutdown requests cancellation only while the active execution remains before finalization, wakes its output with `Stopped`, and leaves the already claimed native lease in place until its synchronous callback reaches rollback or terminal completion.

### 2026-08-14 — I-035: native value output uses a request-owned typed writer

The scheduler records the exact document and database key when AutoCAD claims an execution action. A `println` writer is enabled only while that one claimed job is evaluating a form and only when the native callback's current document and database tokens match the recorded key. The writer receives no execution identifier, sink handle, or routing label from AutoLISP. Outside that state it is inert and the eventual Lisp function still returns `nil`.

The scheduler lock protects only the short routing check and cloning of request-owned I/O state. The resulting Rust writer owns the bounded formatter and output sink independently, so every typed event and any byte-budget wait occurs after the scheduler lock has been released. One tagged CXX entry point carries scalar fields and borrowed bounded text fragments. Storing owned strings in a generic event object was rejected because it would add an allocation and copy at every native fragment; exposing one FFI function per printable type was rejected as unnecessary bridge surface.

`acadctl:println` accepts zero or more top-level values, concatenates their display representations, and adds exactly one newline. The later implicit `eval` writer uses the same event vocabulary in readable mode but requires exactly one top-level value. Malformed structure, a renderer limit, premature output completion, or dropping an unfinished active writer latches the first bridge failure on the execution. At the form checkpoint, an actual evaluator failure takes precedence over that bridge failure, the bridge failure takes precedence over concurrent cancellation, and the selected failure follows the ordinary rollback path with the form's source location. Disconnect, accepted cancellation, and plugin stop terminate traversal without being reclassified as bridge corruption.

A process-global current sink, a C++-owned request pointer, caller-supplied correlation, native formatting policy, and whole-value rendered strings were rejected. This slice exposes only the private trusted writer boundary; per-document `acadctl:println` registration and mechanical native value traversal remain separate proof slices.

### 2026-08-14 — I-036: every value writer is leased to one form generation

Each yielded form opens a new generation in its execution-owned output state. The exact-document routing check may claim one writer lease only while that generation is open, has no prior bridge failure, and has no other live writer. Finishing or terminalizing the writer releases the lease. The form checkpoint closes the generation before it selects evaluator failure, bridge failure, cancellation, or continuation; any lease still outstanding at that point becomes an `Abandoned` bridge failure attributed to that same form. Older writers then become inert and cannot write output or record a fault against a later form.

Relying only on the future C++ callback to destroy its writer before `complete_execution_step` was rejected. The CXX boundary transfers an owning box but its type alone does not prove callback ordering, so a retained writer could otherwise fail after the final checkpoint and allow success, or contaminate the next form's failure slot. The generation and outstanding-writer count make that lifetime rule authoritative in Rust while retaining a short bridge-state mutex; no output emission or backpressure wait holds it.

A writer releases its formatter, execution I/O reference, and producer sink as soon as it reaches disconnect, cancellation, stop, completion, malformed structure, or a renderer limit. Keeping a terminal writer's list stack and request-owned I/O alive until an arbitrary later CXX box destruction was rejected because the native boundary does not need that state after its first terminal result.

### 2026-08-14 — I-037: Rust normalizes native binary reals

I-028's numeric boundary is amended: ObjectARX exposes real and point values to the registered function as binary `double` fields, so those fields cross the mechanical bridge unchanged and Rust owns their textual normalization. Requiring C++ to manufacture an already normalized token would move printer policy into the bridge and would depend on a native formatter whose contract is not AutoLISP `prin1` or `princ` semantics.

Live AutoCAD 2027 checks established the complete notation boundary used here. Real output is six significant decimal digits with ties-to-even rounding; scientific notation is selected from the rounded exponent when it is below -4 or at least 6; fixed and scientific integral mantissas retain `.0`; scientific exponents use lowercase `e`, an explicit sign, and at least two digits; negative zero becomes `0.0`. The checked corpus includes both notation thresholds, rounding carries across those thresholds, positive and negative half cases, and the largest and smallest normal `double` values. The pure Rust formatter derives the notation from an already rounded scientific representation rather than `log10`, so powers of ten and rounding carries do not choose the wrong branch.

Non-finite bridge values are rejected as malformed events. AutoLISP's reader collapsed tested overflow and subnormal literals to zero, but that did not prove how its printer would spell a raw subnormal injected through native code; finite subnormal spelling therefore remains a live native-domain gate rather than being silently clamped. Rust shortest-decimal output, `acdbRToS`, locale-sensitive native formatting, and treating reader overflow as evidence that infinity means zero were rejected because none match the observed printer contract reliably.

### 2026-08-14 — I-038: `acadctl:println` is registered per drawing and walks documented native values mechanically

The public external function is defined during each `kLoadDwgMsg` edit session and undefined during the corresponding `kUnloadDwgMsg`, matching ObjectARX's per-document AutoLISP symbol lifecycle. A fixed application-local function code identifies the callback. Native capability is tracked against the exact document and current database generation and is cleared conservatively when that database changes. Failure to define or bind it marks execution unavailable for that drawing but does not abort the drawing's load or disable unrelated status and lifecycle operations. A later user definition can still shadow the external function, so the `acadctl:` namespace remains a contract rather than a native enforcement mechanism.

The callback derives the current document and database tokens from AutoCAD and asks Rust for a writer. It accepts no caller-supplied request identity. An inactive writer makes the function a no-op. AutoLISP `nil` is published before the form writer lease is released; success returns the external-function result code `RSRSLT`, while failed result publication records a bridge failure on that form and returns `RSERR`. Command-processor status `RTNORM` is not itself a registered-function return code. The argument chain returned by `acedGetArgs` is consumed synchronously and never retained or freed by the plugin. Strings are discovered and transcoded in bounded chunks without first scanning their full length and without splitting UTF-16 surrogate pairs on platforms that use them, so cancellation and terminal output state are checked between chunks. Entity names are converted only to stable database handles; selection-set payloads are omitted until their native identity representation is proved. Unknown and pointer-sized native tags are reported through the stable unsupported-value event without reading or exposing their union payload.

The native walker is iterative and uses constant-memory cycle detection over the `resbuf` chain. A detected cycle becomes a bridge failure and follows the form's rollback path; acyclic values have no arbitrary node-count cutoff and remain governed by bounded output backpressure and cancellation polling on every event. Pre-rendering the entire argument graph, recursive C++ formatting, freeing the `acedGetArgs` chain, exposing native addresses or file paths, and treating `RTRESBUF` as an undocumented pointer were rejected because they would add unbounded copies, move policy into C++, or rely on unsupported ownership and union layouts.

### 2026-08-14 — I-039: the implicit `eval` value is a post-commit phase

The successful `eval` form leaves its raw value rooted only until AutoCAD has positively closed the request's undo group. Rust then yields a distinct `EmitValue` step and opens a new readable, exactly-one-value output epoch. `exec` continues clearing every implicit form value immediately. Handing the `Commit` step to the native loop remains the cancellation point of no return; a cancellation request that loses that race is too late and does not suppress the implicit value.

A failure while reading, visiting, formatting, or clearing the result after `_UNDO End` is an execution failure with `DrawingOutcome::Committed`. It never issues `U`: the drawing group is known closed, and undoing valid mutations because their return value could not be delivered would cross the finalization boundary. Reporting `Unknown` was rejected because it would hide the positive commit evidence and could make an automatic retry repeat mutations. Reporting success was rejected because `eval` did not satisfy its value contract. Already streamed `println` output and a partial implicit-value prefix remain visible; buffering a complete value solely to retract output was rejected because it would violate the bounded-output design. A later document-context restore or unlock failure can still amend any outcome, including `Committed`, to `Unknown`.

### 2026-08-14 — I-040: retained eval state is cleared separately from drawing rollback

Rust marks the one `eval` form step with an explicit retain-value property; C++ does not infer execution mode from source or printer activity. Successful eval evaluation clears the transient source, status, error, and errno symbols while retaining `acadctl:*value*`. Every `exec` result and every evaluator failure clears all reserved symbols immediately.

If eval must fail or cancel before commit completes, Rust yields `ClearValue` before `Rollback`. Cleanup and drawing recovery therefore produce separate evidence: a cleanup failure is preserved in the diagnostic, but a subsequently proved drawing rollback still reports `RolledBack`; only rollback or context-proof failure reports `Unknown`. Folding value cleanup into the native rollback command was rejected because it would make those outcomes indistinguishable.

Form output and post-commit value output use separate generation-tagged epochs. Form epochs permit sequential one-at-a-time `acadctl:println` writers. The eval epoch requires exactly one writer claim and cannot be reopened after release, including after disconnect or plugin stop. Closing an epoch invalidates stale writers before evaluating its result, so a retained native writer cannot emit into or fault a later phase. No scheduler or epoch mutex remains held during formatting or byte-budget backpressure.

### 2026-08-14 — I-041: a private Lisp visitor normalizes the committed eval value

The post-commit value path uses a compile-time embedded AutoLISP visitor rather than `acedGetSym`. The visitor captures `acadctl:*value*` in a lambda local, clears the global root before visible output begins, and iteratively emits typed events through the private fixed-arity `acadctl:_value-event` function. Strings and raw symbol names are sliced into at most 4,096 AutoLISP characters per callback, list traversal uses an explicit stack capped at 4,096 nested values, and Floyd cycle detection replaces a cyclic tail with a stable `#<Object Cycle>` display. Reaching the visitor depth boundary produces `#<Object DepthLimit>` rather than recursing on the native or Rust stack.

The private callback is registered and undefined with `acadctl:println` in every drawing. It accepts no request identity. Rust first validates and claims the exact execution, document, database, post-commit phase, and exactly-one writer epoch. C++ then exposes the owned Rust writer through a thread-local borrowed pointer only for the dynamic extent of the synchronous fixed visitor command; an RAII guard clears it before the step can complete. This narrow reentrant handle amends I-035's rejection of a C++ request pointer: it is neither routing authority nor persistent request state, and source Lisp cannot select or retain it. Keeping the writer in a Rust global, passing an execution ID through Lisp, and storing a whole rendered value were rejected because they would complicate ownership, expose correlation, or violate bounded output.

`acedGetSym` was rejected as the primary value path. Documented result buffers have no symbol tag or generic Lisp-object field, so they cannot preserve arbitrary symbols and opaque leaves reliably; the call also returns a caller-owned materialized result graph proportional to the complete value before streaming can begin. The visitor still necessarily retains the original AutoLISP value while it walks and may allocate a raw symbol-name or bounded substring in AutoCAD. The 256 KiB queue and 16 KiB fragment limits bound Rust/C++ rendering infrastructure, not the source Lisp value or AutoCAD's own representation of one atom.

### 2026-08-14 — I-042: implicit Lisp state is touched only in the proved document context

After every synchronous evaluator or value-visitor command returns, C++ rechecks the exact current document, unchanged active document, and database token before calling any implicit-context symbol or system-variable API. If the check fails, it does not read or clear symbols in whatever drawing became current. When the original document and database are still the current context, terminal cleanup makes one safe best-effort clear before Rust records context loss. Otherwise the execution becomes unknown and the existing context-cleanup/quarantine path prevents later mutations; a reserved Lisp root can then remain until that drawing environment or process is destroyed.

Blindly clearing through `acedPutSym` after context loss was rejected because retaining a private value is safer than modifying an unrelated drawing's Lisp environment. Claiming unconditional cleanup was also rejected: exact routing evidence and cleanup evidence are separate, and the diagnostic must preserve when safe cleanup was impossible.

### 2026-08-14 — I-043: post-commit output retains ordinary pipe backpressure

Once commit handoff wins, cancellation cannot roll back the drawing or suppress the logical eval value for a still-attached client. A connected reader that stops consuming can therefore block the AutoCAD thread at the bounded output budget, like an interpreter blocked on a full stdout pipe. The eventual RPC implementation must drain output concurrently with control messages; disconnect and plugin stop wake the producer and switch it to discard. If Ctrl+C receives `TooLate` after the commit boundary, the CLI detaches instead of waiting for a rollback that can no longer occur, which closes its response stream and releases backpressure while the accepted job finishes internally.

Dropping output merely because a connected reader is slow and adding a post-commit cancellation timeout were rejected because they would violate byte-exact interpreter output and the no-runtime-timeout contract. Live slow-reader, stopped-reader, detach, disconnect, and process-kill tests remain public release gates.

### 2026-08-14 — I-044: scheduler quarantine is independent of drawing outcome

Failure to clear reserved AutoLISP evaluator state prevents later mutations in that AutoCAD process, but it does not erase drawing evidence already established by the undo transition. A request whose group was positively committed remains `Committed`; a request whose rollback was positively completed remains `RolledBack`. The native result therefore distinguishes retained private execution state from an unproved document or undo lease. Both conditions quarantine the mutation scheduler, but only the latter amends the drawing outcome to `Unknown`.

Reusing the execution-lease failure result for both conditions was rejected because it made process safety look like drawing uncertainty and could encourage an unnecessary retry of already committed mutations. Allowing later jobs after cleanup failure was also rejected because a reserved root could contaminate another execution in the same Lisp environment.

### 2026-08-14 — I-045: Rust owns the private visitor protocol and error precedence

I-041 is amended so Rust is the sole authority for the committed-value visitor's event codes, traversal depth, and chunk size. Rust renders the embedded Lisp template from named markers at compile time, validates every raw event code and payload combination, assigns stable cycle and depth-limit displays, and owns the writer's disconnect, stop, cancellation, structural-failure, and cleanup precedence. The current Lisp substring boundary is 2,048 characters, chosen by Rust so one callback remains bounded even on a platform whose native character representation uses UTF-16 surrogate pairs.

The Lisp visitor retains only operations that require access to an actual Lisp object: `type`, cons traversal, Floyd cycle detection, `vl-symbol-name`, and bounded `substr`. It does not format a composite value, contain numeric copies of the Rust event enum, decide display labels, or scan a complete string with `strlen` before the first cancellable chunk. C++ validates the fixed two-argument callback shape, reads only documented `resbuf` union members, converts a bounded string fragment or entity name to raw text, and forwards `{code, payload kind, scalar fields}` to Rust. Reusing the public native-value enum for the private Lisp codes was rejected because it created two incompatible numeric schemas and moved protocol validation into the bridge.

The private callback keeps one fixed `acadctl:_value-event` binding per drawing, registered with `acadctl:println` during the documented drawing-load lifecycle. This name is protected by the existing contract that the complete `acadctl:` namespace is reserved; it is not a hostile-code isolation boundary. Per-execution randomized function names were rejected because runtime registration is not the documented lifecycle, every name can remain interned in a long-lived Lisp environment, and the extra native binding state would not improve supported-source behavior.

Native step observations carry reserved-symbol cleanup status separately from the evaluator or visitor result. Rust preserves a real Lisp or native failure first, then an output-bridge failure when no primary evaluation failure exists, and finally appends cleanup evidence. C++ replacing the original detail and `ERRNO` with a generic cleanup error was rejected. Impossible disagreement between the Rust step state and the native loop remains conservatively unknown instead of adding a second commit/rollback outcome state machine to C++.

### 2026-08-14 — I-046: execution uses a separately bounded streaming service and shared source bytes

The lifecycle methods and bidirectional Execute method are separate services on the same local transport. Lifecycle requests are limited to 64 KiB; responses retain a 4 MiB bound so a status listing with many server-owned document paths is not accidentally constrained to one request's path limit. Execute requests permit a 5 MiB transport envelope so a 4 MiB source, an optional three-byte UTF-8 BOM, source metadata, and protobuf framing reach the authoritative application check; Execute responses are limited to 32 KiB. Raising the request limit on the existing service was rejected because it would allow multi-megabyte `open`, `save`, and `close` inputs for no execution benefit. The split does not add an IPC version or compatibility layer.

The protobuf source field is `bytes` generated as `bytes::Bytes`. Rust validates UTF-8, rejects U+0000, strips the BOM with a shared slice, scans through borrowed text, and carries clones of the same allocation into native form steps. A protobuf `string`, a `Vec<u8>`, and converting the admitted source into `Arc<str>` were rejected because each can add a full 4 MiB copy while the bidirectional input stream remains alive for `Cancel`. The source name is limited to 4 KiB, document IDs must exactly match the existing six-character alphabet, open paths are limited to 32 KiB, and stored or transmitted diagnostic detail is limited to 16 KiB with an explicit truncation marker.

Admission is bounded across connections, not merely per HTTP/2 connection: at most eight Execute streams may exist while waiting for their first message, validating, queued, running, or returning their terminal response. The scheduler admits at most eight nonempty executions and 32 MiB of source across queued and active jobs. All durable mutation jobs, including lifecycle requests, are capped at 32. A five-second first-message timeout prevents an idle stream from retaining its permit indefinitely. Relying on the transport's per-connection stream limit or an unbounded scheduler queue was rejected because multiple connections could otherwise retain gigabytes of decoded requests or disconnected durable jobs.

### 2026-08-14 — I-047: synchronous admission is the Rust Future cancellation boundary

After the first request message passes bounded validation, a synchronous scheduler call inserts the complete execution job, terminal sender, deadline, source reservation, and output producer under the scheduler mutex before the RPC handler can suspend again. The returned observer owns only the request ID, output reader, and terminal receiver. Dropping the handler, completion future, or response stream cannot remove the job, release native serialization, or imply cancellation. Before admission, the global stream permit is moved into the blocking scanner task and returned with its result; cancelling the handler cannot detach an unaccounted 4 MiB validation task and immediately admit another one.

A separate supervised task reads later client messages so an actual `Cancel` can be processed while outbound HTTP/2 flow control is stalled. EOF, transport failure, task abortion, and response drop are detachment only. Dropping the response directly drops `OutputStream`, which clears buffered output, wakes a blocked producer, and switches later output to discard while the scheduler-owned job continues. A spawned output pump and a second response `mpsc` queue were rejected because they would add buffering, prefetch, and a task whose cancellation could consume or strand output. Only a decoded `Cancel` changes execution state; the stream latches an accepted cancellation so repeated Cancel messages remain idempotently accepted after a queued job has been removed.

### 2026-08-14 — I-048: one timer driver owns first-form expiry and busy retry

The five-second execution-start deadline remains authoritative until Rust hands out the first `EvaluateForm` step, not merely until AutoCAD claims the batch callback. Deadline expiry, queued cancellation, and native claim or form handoff all transition scheduler state under the same mutex. Expiry can therefore win while the execution is queued, while `_UNDO Begin` is in flight, or between a successful Begin and the first form; once a form step wins, the deadline is permanently inactive. A queued expiry finishes output and the terminal result after removing the job, while an active pre-form expiry closes any positively opened empty undo group before reporting `NotStarted`.

One off-main-thread driver watches the earliest actionable execution-start deadline and the FIFO head's readiness retry. A retryable busy result keeps the same head, waits first 50 ms and then with bounded exponential backoff up to 500 ms, and can be advanced immediately by normalized document, command, or Lisp readiness events. The driver requests only the existing coalesced application callback; it never sleeps or polls on AutoCAD's main thread. One task per request, immediate callback retry, and using a notification as deadline truth were rejected because they add cancellation races, wake storms, or main-loop spin.

### 2026-08-14 — I-049: the response stream is ordered, directly backpressured, and cancellation-aware

An admitted response yields `Accepted`, then streamed output and any cancellation acknowledgement, drains the execution output to its terminal state, yields exactly one `Finished`, and ends. `Finished` never races ahead of queued output. The earlier conceptual `Success { value: Option<String> }` is amended by I-039: the implicit eval value is ordinary bounded `Output`, and terminal success carries no duplicate value. Expected validation, capacity, busy, AutoLISP, rollback, and cancellation outcomes remain structured terminal events; malformed stream usage and a service failure that cannot form a terminal event use non-OK transport status.

The server reports `Cancel` as `Accepted` or `TooLate`, allowing the CLI to distinguish rollback from post-commit detachment. The response stream owns and polls the bounded output reader directly. After yielding every server event it deliberately returns `Pending` before removing another chunk, preventing Tonic's default encoder batching from pulling two 16 KiB chunks out of the application budget in one poll. Prefetching output, racing completion against output, omitting the `TooLate` observation, and reconstructing an eval value in the terminal message were rejected because they break byte ordering, bounded single-flight accounting, or the recorded Ctrl+C behavior.

The 4 MiB source contract still has no hidden top-level-form count cap. Validation runs in the bounded blocking pool so a maximum source does not stall the single-thread RPC runtime, while the native execution lease retains incremental constant-memory scanning. The pathological millions-of-forms case and the main-thread cost of locating a maximum-size next form remain explicit live measurement gates under I-007; an arbitrary admission limit is not introduced without those measurements.

### 2026-08-14 — I-050: one shared reservation spans the complete execution lifetime

I-046 is refined so its eight-request limit is one reference-counted reservation pool, not two independent limits for attached RPC streams and durable scheduler jobs. The RPC handler acquires a reservation before it polls the first message. Validation retains it even if the handler future is dropped. Admission gives the scheduler job a shared reference while the response keeps its own reference; dropping an attached response therefore cannot free a slot while its accepted execution remains queued or active, and native completion cannot free the slot while a terminal response is still attached. The slot returns only after both sides release the request.

Separate eight-slot semaphores were rejected because eight detached 4 MiB jobs could coexist with another eight decoded or scanning requests, doubling the intended source-heavy admission budget. The scheduler still checks admitted execution count and aggregate source bytes defensively, while the shared reservation is the authoritative across-connection boundary before decoding.

### 2026-08-14 — I-051: the application output budget and transport buffers are accounted separately

I-025 and I-049 are amended: a Tonic response stream does not expose an acknowledgement that one encoded message has left HTTP/2, so returning `Pending` once after each event cannot prove one transport chunk in flight. The direct response still owns no secondary application queue: it removes at most one 16 KiB string for each stream poll, and the request-owned output channel retains its 256 KiB payload budget. After that boundary, the pinned Tonic/Hyper stack may hold codec frames and the pinned h2 server permits approximately 400 KiB of buffered send data per stream, plus bounded kernel socket buffering. The shared eight-request reservation bounds the aggregate number of these response paths. Exact allocator and socket resident memory remains a measurement gate.

The artificial `yield_now` between response events was removed because it prevented same-poll codec batching but could not represent a send acknowledgement or change the HTTP/2 bound. A custom Hyper server or an acknowledgement protocol solely to restate Tonic flow control was rejected as disproportionate transport architecture. Output ordering and cancellation remain Rust-owned; HTTP/2 and the local socket provide the final bounded backpressure layer.

### 2026-08-14 — I-052: diagnostic composition and native capture are bounded by Rust policy

The 16 KiB diagnostic boundary applies after every Rust composition, not only at native ingress and final protobuf encoding. Appending rollback, cleanup, or context evidence re-applies the same UTF-8-safe truncation with one explicit marker, so scheduler-owned outcomes never retain a larger combined message. The RPC adapter reuses that domain helper rather than maintaining a second truncation algorithm.

AutoCAD still owns the original Lisp error string and `acedGetSym` may materialize its native result buffer in proportion to that runtime value. The avoidable bridge copies are bounded: Rust supplies the capture boundary, C++ scans and transcodes only that bounded prefix, and Rust performs the final byte-exact truncation. Putting the diagnostic limit or final marker policy into the embedded Lisp or C++ was rejected because error presentation belongs in Rust; copying the complete native string before truncation was rejected because it added unbounded bridge amplification without improving the diagnostic.

### 2026-08-14 — I-053: transport concurrency bounds response buffers after the application stream ends

I-051 is corrected: the shared execution reservation ends when both the scheduler job and application response state are gone, but Tonic may finish polling that response before h2 has flushed its buffered DATA. The reservation therefore does not itself bound every retained transport response path. In the pinned h2 implementation a closed response continues to count against the connection's concurrent-stream limit until its pending send data and buffered DATA are empty.

The local server accepts at most nine connections and one concurrent HTTP/2 stream per connection. This bounds retained h2 response paths to nine. Eight connections match the execution reservation limit; one remains as headroom for `ls` or a lifecycle request while all execution slots are occupied. Each path can retain the approximately 400 KiB pinned send-data cap described by I-051, one body frame bounded by its service's encoding limit, and bounded socket buffering; the 4 MiB lifecycle response limit makes a maximum `ListResponse` frame materially larger than an Execute frame and remains part of the resident-memory measurement gate. One stream is sufficient because every CLI operation opens its own local connection and Execute carries its request, Cancel, output, and terminal result bidirectionally on that same stream. Additional multiplexing would only add buffering in front of one serialized AutoCAD mutation lease.

Nine idle or stalled local connections can occupy the complete accept capacity and delay another caller indefinitely; a socket may enter the OS backlog before the server accepts it, so the connector's timeout alone is not an end-to-end operation timeout. That bounded availability tradeoff is accepted for the current ephemeral, one-command, same-user clients. Saturation recovery and a client-side pre-admission timeout remain gates; neither is an execution runtime timeout after `Accepted`. A future persistent-channel API would need idle-connection lifetime policy or active-stream admission instead of raising this raw connection cap. An application receipt after `Finished`, keeping a logically completed response open until the client drops it, and replacing Tonic's response body with a transport-aware body were rejected. Each would couple the execution protocol to HTTP/2 drain mechanics or give ordinary clients surprising terminal behavior when the same bound follows from the server's existing connection and stream controls.

### 2026-08-14 — I-054: names describe ownership boundaries rather than the current call site

A first-principles naming sweep is applied before the CLI and history surfaces make the private vocabulary expensive to change. Public `eval` and `exec` remain the two user-visible semantics, while `Execute` remains their one shared streaming RPC verb. Renaming that RPC to `Run` was rejected because it would add a third execution verb without distinguishing a new operation.

The monotonically assigned scheduler identifier is a `MutationJobId`, carried across C++ as `job_id`. It identifies one scheduler-owned FIFO item, including document lifecycle jobs, and is not a transport request ID or a provenance-bearing execution identity. The name `ExecutionId` remains reserved until native history evidence proves what one execution owns. Reusing the scheduler counter for future undo ownership was rejected because a job can finish without creating a drawing-history step and lifecycle jobs are not executions.

The document and execution RPC adapters are named `DocumentService` and `ExecutionService`. Their stream envelopes are `ExecutionClientMessage` and `ExecutionServerEvent`; cancellation distinguishes `ExecutionCancelRequest`, `ExecutionCancelAcknowledgement`, and `ExecutionCancelDisposition`. The transport still has no protocol version or compatibility branch. Keeping a project-wide `Acadctl` service and an `Executor` service was rejected because the former hides the document boundary and the latter sounds like an async runtime rather than an execution protocol.

Public document state separates `display_name` from optional `file_path`. An unnamed drawing therefore has a title but no invented filesystem path, while named drawings retain both the concise UI name and exact save target. Native publication is a `NativeDocumentSnapshot`, its opaque pointer field is `document_token`, and database-token mismatch is `DocumentGenerationChanged`, not ordinary drawing modification. The snapshot invalidation flag is `stale`, never `dirty`; `dirty` remains reserved for unsaved drawing state. Preserving the former overloaded `path`, `token`, `DocumentChanged`, and dirty-snapshot vocabulary was rejected because it would conflate UI identity, filesystem provenance, database replacement, and unsaved edits.

The Rust `scheduler` module remains the single serialization authority for all current and future mutations, but its state and entries are `MutationScheduler` and `MutationJob`. Renaming the module to `mutation_scheduler` was rejected because callers should depend on one scheduler boundary rather than anticipate several schedulers. `ExecutionAdmission` and `ExecutionReservation` remain unchanged because they already identify distinct ownership transitions. The five-second boundary is an execution start deadline: it remains active through undo-group begin and ends only when the first form is handed out.

Execution-domain names are explicit at module boundaries: `ExecutionOutcome`, `ExecutionFailure`, `SourceValidationError`, `ExecutionStepKind`, `ExecutionStepResult`, and `ExecutionStepResultKind`. Native steps are `BeginUndoGroup`, `EvaluateForm`, `CommitUndoGroup`, `EmitEvalValue`, `ClearRetainedEvalValue`, `CloseEmptyUndoGroup`, and `RollbackUndoGroup`. `Abort` was rejected because that step positively closes an empty owned group and never issues `U`. The internal cause is an `UnwindCause`, not a rollback cause, because it also drives empty-group closure before any form runs. Native cleanup failures distinguish general `ExecutionCleanupFailed` from `EvaluatorStateCleanupFailed`; C++ variables that track possible group ownership say `undoGroupMayBeOpen` rather than claiming to represent the complete execution lease.

Transport limits name their service and direction: drawing path, document request/response, and execution request/response. The `acadctl-lisp` crate, output/value types, `NativeAction`, `DocumentRegistry`, `DocumentTarget`, and `NativeDocumentKey` retain their names. Renaming the source scanner crate to `acadctl-lisp-source`, renaming isolated embedded Lisp files, or changing established output types was rejected as churn without a future collision. The crate already has a deliberately narrow API and no runtime ownership despite its concise name.

The eager `SourceBatch` and `SourceShape` examples in the original design are superseded by I-007's validated incremental scanner and constant-size `ScanPosition`. The original monolithic `ExecutionState` sketch is likewise no longer naming authority: transport admission, mutation-job state, native execution phase, execution outcome, and drawing outcome are separate layers. They remain visible above as design history rather than being rewritten after implementation evidence changed the architecture.

### 2026-08-14 — I-055: implementation checkpoint after the naming sweep

The committed implementation now covers the lexical scanner, document-generation identity, scheduler-owned mutation jobs, private execution lease, bounded output, typed value rendering, request-routed `acadctl:println`, post-commit eval value emission, cancellation state, and bounded bidirectional Execute RPC. The architecture naming sweep is the final refactor before a public execution client consumes those APIs.

I-056 through I-060 completed the CLI execution surface, and I-068 replaced provenance-safe history with ordinary drawing-wide undo and redo. This checkpoint remains only as implementation history.

### 2026-08-14 — I-056: the CLI validates one bounded in-memory source before contacting AutoCAD

The public `eval` and `exec` commands now share one CLI runner and retain their distinct mode at the request boundary. A missing file argument and `-` read stdin; another argument is opened locally. Terminal stdin prints the platform EOF instruction to stderr and still collects the complete source before document lookup. The CLI rejects a non-UTF-8 path because that path is also the diagnostic source name carried by the protocol, and it bounds that name to 4 KiB.

Input is read through a 64 KiB scratch buffer and retained only through 4 MiB plus an optional three-byte BOM and one oversize probe byte. BOM removal is a shared-buffer slice. UTF-8, U+0000, lexical completeness, and the `eval` one-form rule are checked locally with `acadctl-lisp`; the plugin repeats the authoritative checks before admission. The 4 MiB constant lives in `acadctl-rpc` because it is the shared request boundary used by the CLI, protobuf service, and plugin domain adapter, while scanner and execution policy remain outside that crate. Reading an unbounded file and relying on Tonic's total-message rejection were rejected because they give late, transport-shaped errors and permit avoidable memory growth.

Document resolution and connection occur only after local source success. The execution connection and the initial server response each have a five-second client boundary. The latter is disabled once Ctrl+C has requested cancellation so the client cannot tear down the only cancellation handoff merely because the pre-admission wait elapsed. Neither boundary applies after the server starts the response, and neither is an AutoLISP runtime timeout. A timeout before the response is reported as an unknown request handoff and explicitly forbids blind retry because server admission may have won immediately before transport observation.

### 2026-08-14 — I-057: safe detachment requires a cancellation receipt and cancellable local stdout ownership

The original second-Ctrl+C rule is refined. The first signal sends the one allowed `Cancel`, stops waiting on any blocked local stdout write, discards later output locally, and keeps polling the same response. The second signal requests detachment, but the CLI exits only after `CancelAcknowledgement` proves that the plugin scheduler accepted the cancellation, or after the server reports that cancellation was too late. This receipt wait does not wait for the current Lisp form, rollback, or terminal outcome; on the local transport it is normally one control round trip. A third Ctrl+C is the explicit unsafe escape when even that control path is unavailable: it exits with status 130 and states that cancellation was not confirmed and the accepted job may still be running.

Immediate process exit on the second signal and guaranteed durable cancellation cannot both be provided by one bidirectional stream: dropping the runtime can discard a locally queued Cancel before the server's control reader latches it, while disconnect alone deliberately does not cancel accepted work. A hidden helper process, a second public cancellation RPC with transferable job identity, and claiming that a locally polled HTTP/2 body frame is a server receipt were rejected as larger or false mechanisms. The acknowledgement is the existing authoritative linearization evidence. A persistent platform signal stream is created before the request is exposed, eliminating the gap between independently armed one-shot Ctrl+C futures.

Stdout is owned by one lazily created operating-system thread with a one-request channel and a completion receipt per output chunk. Normal execution still writes and flushes each received chunk in order before taking the next. If the pipe blocks, cancellation drops only the asynchronous receipt wait; no Tokio blocking-pool task remains for runtime shutdown to join, so confirmed or forced detachment can terminate the process. Tokio's asynchronous stdout wrapper was rejected here because its started blocking write cannot be cancelled and the macro-owned runtime waits for that task during shutdown, making Ctrl+C ineffective against a full pipe. A stdout error remains observer detachment rather than cancellation intent, so the diagnostic states that the accepted AutoCAD job may continue.

### 2026-08-14 — I-058: execution diagnostics cannot own the async control thread

The stdout isolation in I-057 was incomplete when stderr shared the same stopped or full pipe, as with `2>&1`. Synchronous cancellation notices on the current-thread Tokio runtime could then block before the queued Cancel reached HTTP/2, prevent response polling, and prevent later signals from reaching the detach state machine. Execution-time stderr therefore has its own operating-system writer thread and bounded queue, parallel to stdout but independently owned.

Ordinary terminal diagnostics carry a completion receipt so a writable stderr still receives the message before the CLI returns. That wait remains interruptible: a signal advances the same cancellation/detachment state instead of waiting on the pipe. Progress notices and the final confirmed or forced-detach notice are best effort because observability through a blocked stderr cannot be made a prerequisite for the control transition they describe. At most one stderr write is active and eight messages are queued, so repeated notices cannot create an unbounded observer buffer. Errors produced before any execution request is exposed—source intake, target lookup, connection setup, and signal-handler installation—retain the simple direct stderr path because no accepted job or cancellation handoff can be starved there.

Using Tokio's blocking pool for stderr, treating blocked stderr as outside the cancellation contract, and synchronously printing a final detach notice were rejected. The first recreates runtime-shutdown coupling, the second makes a common combined-pipe composition unsafe, and the third turns a diagnostic about escape into a new reason escape can hang. The regression boundary holds both writer threads blocked while three synthetic interrupts still reach unconfirmed detachment; live process tests with a real combined pipe remain part of the platform gate.

### 2026-08-14 — I-059: local interrupt progress is independent of Cancel transport availability

The signal task records two separate facts for the first Ctrl+C: whether the one allowed Cancel was queued for the outbound request stream, and that the local user interrupt occurred. It always publishes the latter to the CLI state machine even when Tonic has already dropped the outbound receiver. The notice says that cancellation could not be sent when those facts differ. Coupling the local event to a successful outbound send was rejected because response loss could close that channel while a diagnostic was blocked on stderr; the installed signal handler would then consume every later Ctrl+C without waking the control task.

Stdout start and write failures use the same interruptible stderr completion receipt as other ordinary terminal diagnostics. Best-effort queueing remains limited to progress and final-detach notices. Returning immediately after merely enqueueing the sole unknown-outcome diagnostic was rejected because detached operating-system threads are not joined at process exit and the message could be lost even on a writable pipe.

### 2026-08-14 — I-060: observer failure disconnects output before reporting

A stdout writer start or write failure first drops the live Execute response and only then waits for the stderr diagnostic receipt. Retaining the response while stderr was blocked would retain the plugin's `OutputStream`; its 256 KiB queue could fill and leave AutoCAD's main thread blocked in `acadctl:println` even though the client had already lost its output observer. Closing the response makes server-side output switch to disconnect/discard and is independent of whether the diagnostic pipe drains. The execution itself remains detached rather than implicitly cancelled, and the message keeps the unknown-outcome and no-blind-retry warning.

### 2026-08-14 — I-061: native-state quarantine survives transport restart

The mutation scheduler's process-level quarantine is monotonic for the lifetime of the loaded plugin. Starting or restarting the local RPC server clears only the scheduler's stopping flag; it cannot clear quarantine, reconstruct execution state, or make native mutation safe again. The scheduler begins unquarantined from its fresh-process initializer, and only a new AutoCAD process can restore that initial state.

The earlier `start` implementation reset both stopping and quarantine before it even checked whether the RPC server was already running. That coupled transport availability to native lease recovery: a repeated server-start call could admit later drawing mutations after an unproved context, undo-group, or evaluator-state cleanup failure. Retaining that behavior was rejected before adding history provenance because a transport lifecycle has no evidence that the AutoCAD process recovered. Test-only scheduler reset remains explicit and cannot be called through the production bridge.

### 2026-08-14 — I-062: history ownership requires a live native trace predicate

The installed ObjectARX 2027 interface exposes command and Lisp start/terminal callbacks, database mutation callbacks, undo subcommand callbacks, redo counts, raw undo-boundary values, and the number of subcommands about to be undone. It does not expose a supported top-record identity or a public undo-stack query. The header also refers the raw boundary value to `dbundo.h`, which is absent from the installed macOS SDK. `UNDOCTL`, command success, group Begin/End, database mutation, or any one callback is therefore insufficient ownership evidence.

Default history remains unavailable until a fresh AutoCAD process and disposable drawings establish a unique event predicate for an empty group, a read-only or Lisp-only group, one mutating group, a grouped multi-form mutation, safe one-step undo and redo, native absence, foreign history activity, and database replacement. The trace must include the current, event, and MDI-active document/database tokens so inactive-document behavior cannot be inferred from process-global callback order. If those sequences are not distinguishable without issuing a speculative history command, safe undo and redo do not ship; blind `U` followed by repair, command-status inference, `DBMOD` inference, private `dbundo.h` constants, and drawing sentinels remain rejected.

### 2026-08-14 — I-063: provenance is a bounded Rust suffix with late native authorization

The conceptual `OwnedStep` above is amended before implementation. Production provenance is keyed by the exact `NativeDocumentKey`, lives inside the existing `MutationScheduler` mutex, and retains bounded undo and redo suffixes containing only a distinct `ExecutionId`. `MutationJobId` is not reused. Mode and source name are omitted because neither participates in authorization and retaining a 4 KiB source name per step would make a long-lived drawing's tracker unnecessarily large. Reaching a suffix or tracked-document cap evicts knowledge only by moving the implicit unknown barrier upward; it never makes an older step safe.

There is no second history queue, provenance mutex, or per-object Rust callback. Native database subscriptions coalesce high-frequency object activity into fixed bitmasks and drain them at existing command, Lisp, document, evaluator, and history boundaries. Sparse editor, undo, redo, system-variable, and lifecycle events cross synchronously as raw fixed data. C++ may sample the current document/database tokens, preserve raw integer arguments whose meaning is undocumented, establish and restore the physical document lease, and issue the one fixed native command selected by Rust. It does not classify owned versus foreign activity, retain a suffix, parse command names for policy, authorize `--force`, compose user diagnostics, or add a Lisp history shim.

A safe history job snapshots its expected top step at the FIFO head, but that is not command authority. After C++ has locked and revalidated the exact document, proved no active command or undo group, and drained pending database activity, it calls back into Rust immediately before the native history command. Rust atomically rechecks the active job, exact key, provenance revision, and expected top step and marks the action issued. No state-changing native call may intervene. Completion moves one step only after command status, reactor evidence, exact context restoration, and unlock all agree. Forced history clears provenance no later than this issuance point even if the native command later reports absence or failure.

### 2026-08-14 — I-064: physical commit evidence, not RPC success, determines history ownership

History tracks the drawing's proven native top step, not whether every later observer contract succeeded. If an eval group is positively proved committed and produced one undo record, that record becomes the new owned top even when post-commit value emission or evaluator-state cleanup makes the RPC outcome a `Committed` failure. Keeping an older owned step recorded as the top would be unsafe, while clearing positive ownership would be an unnecessary false negative. A context restoration, unlock, group-state, or event-correlation failure still clears provenance because the physical top is then unknown.

Failed or cancelled execution remains a harder gate. The current rollback closes the group and issues `U`; if ordinary `REDO` can resurrect that failed request, the drawing rollback contract itself is not acceptable and provenance cannot hide the defect. Live proof must show either that failed work is not redoable or identify a supported recovery sequence that invalidates only that redo entry without touching prior user history. Until then, rollback conservatively clears history knowledge and mutating execution is not release-ready.

### 2026-08-14 — I-065: the pre-proof ledger cannot create production ownership

The first history implementation is deliberately asymmetric. Rust contains and tests the future bounded suffix transitions, but production code can only reconcile exact document/database generations and invalidate knowledge. Methods that create an owned step, prepare a safe traversal, move a step between undo and redo, or model a forced issue are compiled only in tests until the live predicate in I-062 exists. This makes an empty suffix mean “no positively known step,” never “AutoCAD has no history,” and prevents synthetic transition tests or command success from becoming accidental native authority.

The ledger retains at most 32 exact document generations and 256 execution identifiers per document across both directions. A new owned step evicts only the bottom of the undo suffix, document-cap pressure forgets the least recently retained generation, and either case raises the implicit unknown barrier. Recreating an evicted document entry can begin only with a newly proved top step; it cannot restore forgotten identifiers. Revision identifiers never wrap: exhaustion clears every suffix and permanently disables positive knowledge for that loaded process.

Document snapshot replacement and provenance reconciliation occur under the same mutation-scheduler lock. A preserved document pointer with a different database pointer, a closed generation, plugin stop, native-state quarantine, or any currently unproved targeted native mutation clears the affected knowledge before it could be used. Starting the RPC transport does not clear or reconstruct it. Adding public history commands, production ownership transitions, raw-event meaning, or native undo/redo issue paths in this slice was rejected because each still depends on the authorized live AutoCAD trace.

### 2026-08-14 — I-066: history names describe partial provenance, not a complete ledger

The post-commit first-principles naming sweep supersedes the `HistoryLedger` and `DocumentHistory` names introduced in I-065 with `HistoryProvenance` and `DocumentProvenance`. The state is neither a durable ledger nor AutoCAD's complete history: it deliberately begins behind an unknown barrier, forgets generations, and evicts the bottom of a bounded known suffix. The scheduler field is therefore `provenance`, and the two retained sequences are explicitly `undo_suffix` and `redo_suffix`. Their caps name document generations and owned steps rather than generic documents and history.

The test-only transition names similarly distinguish one physical undo step: `record_owned_undo_step` and `record_execution_without_undo_step`. `ExecutionId`, `HistoryDirection`, `NativeDocumentKey`, `native_target`, the `history` module, and the exact-key reconciliation and invalidation verbs remain accurate. Generic `prepare`, `complete`, `HistoryClaim`, and `force_issued` remain test-only instead of being prematurely renamed into a production traversal protocol. Once live evidence proves the three real phases—FIFO-head expectation, late native authorization, and evidence-backed settlement—the production API will name those phases directly and will not accept a generic boolean “success” as ownership evidence.

### 2026-08-14 — I-067: raw history evidence is a one-way fail-closed boundary before proof

The first native history-evidence boundary does not infer ownership. C++ forwards sparse editor and document callbacks synchronously with only the subject actually supplied by AutoCAD, independently samples current and MDI-active context, and atomically coalesces high-frequency database activity into a Rust-defined bit schema. It does not manufacture a complete event document from ambient context, retain an ordered trace, interpret undocumented arguments, or add an AutoLISP history shim. Rust resolves document-only and database-only subjects against the current exact `NativeDocumentKey`, owns provenance invalidation, and retains only a constant-size sticky summary on the active mutation job.

That summary is diagnostic scaffolding, not the future authorization predicate: it deliberately loses order and repeated-event counts. The disposable native trace probe remains the source of ordered live evidence for I-062. If the trace proves a unique predicate, its exact state machine will be implemented in Rust before any production ownership transition or history command is enabled; otherwise safe undo and redo remain absent.

Database observation failure is durable and fail-closed. C++ reports a typed attachment or detachment operation, raw native status, and the database-only subject; Rust permanently retains the first such failure for the loaded process and refuses to preserve any provenance after it. An attachment failure is retried while the exact generation remains open unless AutoCAD reports the ambiguous `eDuplicateKey` result. Every database reactor enters stable bridge-owned storage before `addReactor`, so no allocation or ownership transfer occurs after AutoCAD could retain its raw address. A failed removal or ambiguous duplicate attachment means AutoCAD may still retain that address, so the application-locked bridge keeps the storage and stops attaching new database reactors, bounding retained callbacks by the set that existed when ownership first became unprovable. Plugin unload is refused until every retained reactor has observed database goodbye. Application-context callbacks are likewise counted from scheduling through return, new wake requests are closed before RPC shutdown, and unload is refused while any callback is queued or running because ObjectARX exposes no cancellation operation for them. The initial document snapshot and native reactor installation complete before the RPC endpoint is published, so the first client cannot observe an artificial empty registry. If native initialization throws after callback installation or later RPC startup fails and cleanup cannot prove ownership released, RPC stays stopped and the inert locked module reports successful load rather than returning an error that would unload its code. Successful removal happens before the final atomic drain and destruction. A goodbye or database-destruction callback tombstones every matching subscription before snapshot code can dereference or reattach it. Dense activity is drained before Rust publishes a new active native job and again before that job completes, so an earlier foreign event cannot be attributed to the next mutation and a final owned event cannot escape its job lifetime. Because the installed headers do not state that reactor removal joins an already-running callback, callback thread identity and teardown overlap remain an explicit live gate before this evidence can authorize history.

### 2026-08-14 — I-068: undo and redo are ordinary drawing-wide AutoCAD history

I-062 through I-067 and the safe-history implementation claims in I-003 and I-004 are superseded as product architecture. The live trace work proved why a provenance layer is the wrong abstraction: ObjectARX exposes no supported stable top-record identity, explicit groups can create records without database mutation, and the observed details are sensitive to host behavior rather than an acadctl domain guarantee. Maintaining an owned suffix would add a second, partial model of history that users already understand as drawing-wide AutoCAD state.

`acadctl undo <id>` and `acadctl redo <id>` therefore perform exactly one ordinary drawing history action, regardless of whether the affected step came from the user, an acadctl execution, or another integration. They have no force flag, count, provenance, ownership claim, revision, trace predicate, or repair sequence. Rust owns the single FIFO operation, direction, exact document-generation target, RPC result, and diagnostic policy. C++ only establishes the physical document context, issues fixed `_.U` or `_.REDO`, restores context, and reports raw status.

The persistence model is the familiar one: save before risky work when the on-disk drawing must remain a recovery point. Save does not truncate native undo history. A successful execution still creates one explicit group. Failure or cancellation closes that group and issues `U`; live AutoCAD proved the resulting group can remain redoable, and that is now documented ordinary history behavior rather than a release blocker. The experimental ownership ledger, raw history-event FFI, ordered trace predicate, redo-barrier investigation, and public safe-history gates are deleted. The native database observers remain only as lifecycle-safe, coalesced document-snapshot invalidation; no history semantics cross that boundary.

### 2026-08-14 — I-069: the installed private build passes the core document-context path

The freshly built and installed AutoCAD 2027 plugin passed live document-scoped execution in a fresh process. `exec` ran multiple forms in order, `acadctl:println` streamed explicit output, and `eval` returned a number, nested dotted list, escaped string, symbol, and entity handle. A failing second form returned AutoLISP's own `bad argument type` detail and removed geometry created by the first form. An inactive target executed successfully while AutoCAD restored the previously active drawing.

One fixed `U`, one fixed `REDO`, and a final `U` moved the same execution-created entity through absent, present, and absent states. The same history action also routed to an inactive target and restored the prior active drawing. A request issued while AutoCAD was still completing the preceding document command failed busy and succeeded after the document became quiescent; it did not cancel or intrude on that work.

This evidence closes the core evaluator, value-output, representative rollback, exact inactive-document routing, and ordinary drawing-history gates for the tested AutoCAD 2027 build. It does not close cancellation, disconnect, blocked-output, maximum-source, lifecycle/unload, platform, or process-termination gates. Those remain required before public release.

### 2026-08-14 — I-070: execution uses one queued document Lisp driver, not a synchronous application callback

This entry supersedes the native mechanics in I-002, I-013, and I-019 while preserving their batch, FIFO, and exact-document requirements. An application-context callback verifies quiescence and the exact target generation, activates the target when necessary, and queues one fixed outer AutoLISP driver in that document. The Rust scheduler keeps the same mutation job active until the driver and its application-context finalizer finish; it does not expose the next FIFO item between forms.

The outer driver repeatedly calls one registered bridge function. Each call either stages one Rust-selected evaluator or value-visitor form and returns `T`, or returns `nil` after Rust reaches a terminal step. Rust owns form order, execution state, cancellation, outcomes, and output. C++ owns only the physical AutoCAD context, undo-group state, bounded symbol transfer, and callback correlation. No application-context callback, explicit document lock, or old synchronous C++ step loop is retained across the batch.

The bridge records outer-driver start, nested Lisp depth, driver end, callback activity, and terminal readiness as separate facts. Entry into the registered execution callback also establishes the outer-driver correlation if AutoCAD's undocumented `lispWillStart` text normalization prevented an exact match. The action finalizes only after the terminal callback and outer Lisp return both occur. Premature termination or any internal bridge failure with an unproved undo group, staged program, value writer, or reserved evaluator state becomes `ExecutionCleanupFailed`, which quarantines later mutation until AutoCAD restarts. The application-context finalizer then restores and verifies the previous active document, refreshes snapshots, completes the same Rust job, and only afterward wakes the next FIFO item.

### 2026-08-14 — I-071: post-implementation names separate Rust ownership from native dispatch

The post-commit naming sweep keeps `MutationScheduler`, `MutationJob`, `MutationJobId`, and `NativeAction`. The first three name Rust-owned state. `NativeAction` names one bounded instruction that Rust hands to the native bridge; it does not imply a second C++ scheduler. `RunExecution` is replaced by `QueueExecutionDriver` because the native handoff queues the document driver and leaves execution ownership in Rust. The Rust function that admits an operation is `submit_operation`, so “dispatch” remains available for the C++ callback lifecycle.

C++ uses `PendingDocumentDispatch` for the one queued or finalizing document-context handoff. Its registered ARX command and callbacks name `HistoryCommand`; execution names `ExecutionDriver`, `advance`, and `StagedFormKind`. The reserved symbol is `acadctl:*staged-form*`, not a generic program. `acadctl:_drive-execution` owns only the Lisp loop, while `acadctl:_advance-execution` requests the next Rust-selected stage. These names expose the boundary: C++ and Lisp move bounded physical data and lifecycle facts, while Rust selects steps and outcomes.

Failure names state which safety proof failed: `DocumentContextFailed`, `DocumentContextRestoreFailed`, `ExecutionBridgeFinalizationFailed`, `EvaluatorSymbolsClearFailed`, and `NativeMutationStateUnknown`. Document publication is `publish_document_snapshot` at FFI, `replace_document_snapshot` in the scheduler, and `replace_snapshot` in `DocumentRegistry`; none suggests that Rust replaces live AutoCAD documents. The embedded files are `form-evaluator.lsp` and `eval-value-visitor.lsp`, avoiding a future collision between the private evaluator and a public `acadctl.lsp` standard library.

The sweep retains `CommitUndoGroup`, `AwaitingCommitUndoGroup`, `PostCommitCancelled`, and `DrawingOutcome::Committed`. In Rust these names mark the semantic cancellation and outcome boundary reached when `_UNDO End` succeeds; renaming them to the literal command would obscure that role. It also retains the concise `HistoryRequest`, `HistoryResponse`, `Operation::History`, and `HistoryDirection`: each public invocation already means exactly one fixed drawing-wide step, so adding `Step` everywhere would not resolve an ambiguity. This entry supersedes the implementation names listed in I-054 and any remaining provenance-oriented naming rationale in I-003 and I-004; I-068 already supersedes that product architecture.

### 2026-08-14 — I-072: process termination is an exact CLI-owned OS operation

`acadctl kill [pid] [--force]` is implemented without RPC, plugin state, C++, or Lisp. The CLI enumerates only actual AutoCAD processes, selects the sole instance when no PID is supplied, requires an exact listed PID when several instances exist, and refuses an absent or non-AutoCAD PID. On macOS, graceful termination uses the normal running-application request with an exact-PID Apple-event fallback; forced termination sends the OS kill operation to that exact PID. On Windows, the cross-compiled path posts `WM_CLOSE` only to top-level windows owned by the selected PID and uses `TerminateProcess` only for explicit `--force`.

Both modes wait until the selected PID disappears. Graceful termination waits at most five seconds and never escalates: if AutoCAD remains open, the command fails and tells the user how to issue a separate explicit forced request. The force path is independently selected and still verifies termination. Putting this operation behind the plugin was rejected because it must remain available when RPC or AutoCAD's main loop is unresponsive.

### 2026-08-14 — I-073: the remaining macOS live gates pass

Live AutoCAD 2027 testing closed the remaining current-platform gates. Cancellation during a multi-form batch returned status 130 at the next checkpoint, skipped the later form, and rolled both test entities back. Killing the attached CLI process after admission did not cancel the scheduler-owned job; its later form and both drawing mutations completed and were observed by a new client. With both stdout and stderr directed to one non-reading pipe and enough `acadctl:println` output to saturate every application buffer, Ctrl+C still reached the server, woke the blocked producer, and rolled the drawing back.

A source of exactly 4 MiB crossed the real CLI, RPC, plugin scanner, and evaluator boundary successfully; one byte more was rejected locally with the specified diagnostic. A deliberately single 4 MiB string reached AutoLISP and produced AutoLISP's own `string too long on input` error, confirming that the application source limit is not falsely presented as a guarantee that every host reader form is valid. Busy admission was already proven in I-069: an action issued while AutoCAD was still completing a document command failed without cancelling that work and succeeded after quiescence.

Process testing used disposable drawings. A saved drawing closed through graceful `acadctl kill` in about 2.5 seconds. With unsaved `test.dwg`, graceful kill waited the full five seconds, returned failure, and left the same process alive; only a later explicit `--force` terminated that exact PID. A separate forced request also terminated the prior exact process and subsequent selection refused the now-absent PID. The Windows CLI path compiles with the pinned target, but native Windows runtime and console-signal behavior remain a platform-specific validation task rather than evidence inferred from the macOS run.

### 2026-08-15 — I-074: public `acadctl:println` accepts one value and reuses the Rust-owned visitor

This entry supersedes I-010 and the public direct-argument traversal described in I-035 and I-038. Live AutoCAD 2027 proved that the external-function argument boundary rejects ordinary symbols, error objects, files, and functions before the registered callback can normalize them. A dotted pair can also arrive in a legacy result-buffer shape that is not safely streamable without retaining or pre-scanning composite state. AutoLISP does not support a user-defined variadic formal list, so neither the direct native walker nor a variadic Lisp replacement provides the promised arbitrary-value contract.

The public function is therefore a fixed-arity AutoLISP wrapper that accepts exactly one value. It stores that value in the reserved evaluator slot, asks a private native callback to claim the current form writer, evaluates the existing Rust-generated iterative value visitor, and calls a second private callback to finish the writer and publish `nil`. The same visitor protocol now normalizes explicit display output and the post-commit readable eval value; Rust still owns event codes, payload validation, formatting, output routing, backpressure, cancellation, and the exactly-one-root rule. C++ only stages the bounded visitor source and holds the writer for the wrapper's dynamic lifetime. An interrupted wrapper leaves that writer on the pending document dispatch so the next execution callback or terminal cleanup can fail and release the correct form lease.

Callers that want a label and value on one line construct one value, normally with `strcat`, `itoa`, or another ordinary AutoLISP conversion. Zero or multiple arguments produce AutoLISP's normal arity errors. Outside an active request the wrapper returns `nil` without visiting or emitting its argument. Whole-value rendering, a C++ composite walker, undocumented result-buffer fields, multiple private event schemas, and a second Lisp formatter remain rejected.

The exact installed build printed a string, symbol, nested dotted list, caught error, file, function, entity, and selection set through this path. It also returned `nil` without output outside a request, reported ordinary too-few and too-many argument errors, preserved implicit readable eval output, and rolled back a mutation before a failing later form.

### 2026-08-15 — I-075: process termination retains platform identity; explicit force uses `SIGKILL` on macOS

This entry supersedes the process mechanics in I-005 and I-072 and narrows I-073's evidence to the current implementation. A numeric PID is only a lookup key and can be reused after selection, so discovery returns a CLI-owned platform process object that survives through selection, termination, and exit observation. `ls`, document targeting, and `kill` share that discovery policy instead of maintaining separate basename and termination registries.

On macOS, discovery retains an `NSRunningApplication` whose bundle identifier is exactly `com.autodesk.AutoCAD` followed by a decimal release number. Graceful termination uses that object's normal application request so AutoCAD can perform its save-and-close lifecycle; the CLI waits five seconds and never escalates automatically. Explicit `--force` immediately re-resolves the PID and requires the same retained application identity before sending `SIGKILL`, then waits until the retained identity is terminated or no longer owns that PID. `NSRunningApplication.forceTerminate()` was rejected after a live call returned success while AutoCAD remained running. The current explicit-force path terminated the disposable instance and a read-only process check found no remaining AutoCAD process.

On Windows, discovery retains a query-and-synchronize process handle and creation time. Graceful window closure and an explicit force reopen are accepted only while the retained handle and creation time still identify the same process; the termination-capable handle is then used for `TerminateProcess`. The path cross-compiles, but basename-only `acad.exe` product classification and real console/process behavior remain Windows runtime gates rather than cross-platform claims.

### 2026-08-15 — I-076: host-cancelled Lisp resumes the Rust-owned unwind

This entry supersedes I-041's description of a visitor writer whose native lifetime is limited to one synchronous command and refines I-070's unconditional quarantine rule for premature driver termination. The public one-value wrapper crosses several registered-function callbacks, so its owned writer remains on the pending document dispatch for that wrapper's dynamic lifetime. A thread-local borrowed pointer only makes the currently retained writer reachable to the private value-event callback; it carries no routing identity or independent lifetime. The finish callback and every interruption path invalidate and release the writer before the dispatch can settle.

An AutoCAD `lispCancelled` event can terminate the queued outer driver after `_UNDO _Begin` without returning through the normal evaluator checkpoint. Treating that event as an ordinary bridge failure would leave the drawing group and reserved evaluator state outside Rust's unwind. The native bridge therefore records the interrupted evaluator or value visitor as a failed Rust step, clears evaluator symbols only in the proved target context, and queues the same fixed outer driver again. The existing Rust execution state then chooses `ClearRetainedEvalValue`, `RollbackUndoGroup`, and terminal cleanup exactly as it does for an ordinary form failure. If the interrupted step cannot be correlated, the bridge abandons it into the Rust failure state; if exact context or rescheduling cannot be proved, finalization fails closed and quarantines further mutation instead of guessing at cleanup.

The claimed public println path revalidates the pending execution phase, target document, current and active document, and database generation before staging, reading, or clearing target execution state. An unclaimed begin discards only the ambient `acadctl:*value*` that the wrapper has just stored and returns `nil`. A mismatch after a writer was claimed invalidates that writer and enters the existing context-loss path without clearing state in another drawing.

The exact installed AutoCAD 2027 build was interrupted with Escape while an infinite second form was running after creating a uniquely sized circle. The client returned AutoLISP's `Function cancelled` failure. AutoCAD then reported the undo-group bit clear and all seven reserved evaluator symbols `nil`; the marker circle was absent, and a fresh request in the same process succeeded. This closes host-level cancellation recovery for the tested build independently of cooperative CLI cancellation.

### 2026-08-15 — I-077: names describe the final ownership and handoff boundaries

This entry supersedes the architecture-significant vocabulary in I-071 and the output-path terms amended by I-074 and I-076. A fresh first-principles review of the committed implementation found the ownership split sound but identified names whose temporal or semantic scope became false in the final queued-driver architecture.

C++ holds one `DocumentContextDispatch` from queueing through running and finalization; calling it pending during the latter phases hid cleanup-critical lifetime. Its methods are `queueDocumentContextDispatch`, `scheduleDocumentContextFinalizer`, and `finalizeDocumentContextDispatch`. Execution is the `ExecutionDriver` dispatch kind, and `advanceCallbackActive` plus `finishAdvanceCallback` describe the one `_advance-execution` callback fact rather than execution policy.

The shared AutoLISP slots are execution-bridge symbols, not evaluator-only state. Native cleanup is therefore `clearExecutionBridgeSymbols`, its mechanical result is `LispBridgeStepResult`, retained-state evidence is `bridgeSymbolsMayBeRetained`, and the Rust/native failure is `ExecutionBridgeSymbolsClearFailed`. The actual private globals use `acadctl:*bridge-*` names; undefined failure sentinels use `_invalid-*`, and the loader temporary is `acadctl:*loader-directory*`. Only `acadctl:println` remains public.

Explicit request output is the `Println` value-output kind and writer policy. `Form` was rejected because it was easily confused with the separate implicit eval form value. The RPC `ExecutionOutput.chunk` field likewise states that one transport event can contain only part of a printed value or line. The message remains `ExecutionOutput` because it is still one ordered output event.

Rust's mutating wake transition is `try_claim_native_action_wake`, not a predicate. Poisoned scheduler access is `SchedulerStateUnavailable`. The first-form boundary is represented by `form_handed_off`, `has_handed_off_form`, and `execution_has_not_handed_off_form`, because Rust can prove that it yielded `EvaluateForm` but cannot claim native evaluation already began. The associated five-second timer is consequently the execution-start deadline, beginning at accepted admission and ending at that handoff.

Message-scoped request fields remain `id`, and `NativeExecutionStep` and `NativeValueWriter` retain `Native` because they are opaque work objects crossing the CXX boundary. Renaming those objects or every request field was rejected as churn that would erase useful boundary context without changing ownership.
