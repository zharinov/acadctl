# `acadctl` agent-facing AutoLISP standard library

Status: proposed. This plan extends the execution design in [plan.md](plan.md). It does not change document routing, `eval`, `exec`, drawing rollback, ordinary AutoCAD undo and redo, or save behavior.

## Decision

`acadctl` will provide a batteries-included AutoLISP standard library whose primary caller is an autonomous agent. A human states the drawing outcome. The agent works against the selected document and admission-time context. Preconditions protect each operation, and observed postconditions decide whether the work is complete or needs correction before saving.

The library should replace the general-purpose AutoLISP utility toolboxes that users have accumulated for selection handling, traversal, layers, blocks, attributes, styles, layouts, xrefs, common geometry, batch edits, cleanup, validation, and reporting. Organization-specific rules and unsupported vertical-product semantics remain recipes composed by the agent from the standard library and ordinary AutoLISP.

The public promise is:

> `acadctl` gives an autonomous agent a discoverable, deterministic, recovery-aware interface for inspecting, changing, verifying, and correcting the documented semantic contents of one live AutoCAD drawing. It reports unsupported, unavailable, opaque, failed, skipped, partial, stale, and conflicting cases explicitly.

This promise does not include screenshots, rendered pixels, application chrome, modal UI, Sheet Set Manager data without a provider, eTransmit completeness, unknown external dependencies, or semantic editing of custom objects without an installed provider.

## Current substrate

The repository state on 2026-08-14 provides the execution substrate, not the proposed library:

- The CLI exposes `ls`, `open`, `save`, `undo`, `redo`, `close`, `eval`, and `exec` in [main.rs](crates/cli/src/main.rs).
- `eval` evaluates one top-level form and prints its value. `exec` runs zero or more forms without implicit value output. Each nonempty request is one ordinary AutoCAD undo group.
- `save` is the durable checkpoint. `undo` and `redo` perform one ordinary drawing-history action without acadctl-owned provenance.
- There are no public `acadctl:*` Lisp functions. `acadctl:_value-event` is private `eval` result plumbing in [host.cpp](crates/plugin/native/host.cpp).
- The existing value bridge carries lists, dotted pairs, symbols, strings, integers, reals, points, entity names, selection sets, VLA objects, files, functions, error objects, and unsupported native values. See [value_bridge.rs](crates/plugin/src/execution/value_bridge.rs).
- The six-character public document ID follows the native document token. The internal target also includes a database token in [documents.rs](crates/plugin/src/documents.rs). A database replacement can preserve the document ID while invalidating every object handle.

This plan proposes every `acadctl:*` function below unless it identifies the function as existing.

## Agent interaction model

The standard flow is:

```text
human request
  → capture document and human context
  → inspect normalized facts and provider coverage
  → form a change with epoch and fact preconditions
  → execute one bulk mutation
  → inspect the same facts again
  → compare the observations
  → correct, finish, undo, or save
```

The API follows these rules:

- Document scope is implicit because `eval` and `exec` already target one document.
- Structured Lisp values are the computational interface. `acadctl:table` is the sole table renderer and returns a string like every other computational helper.
- Ordinary Lisp supplies iteration, filtering, branching, aggregation, and composition.
- The schema registry exposes fixed public operations. There is no public generic query endpoint.
- Agents use batch calls when they avoid round trips or repeated native object opens.
- Inspection calls do not make intentional drawing changes.
- Mutation calls state their effects, preconditions, failure policy, and destructive requirements.
- Provider coverage is data. An unsupported provider never appears as an empty successful result.
- Built-in AutoLISP remains available when it is already precise and composable.

## Common data contracts

### Results

Discovery, inspection, paging, checker, and mutation helpers return a result alist unless their failure policy raises a structured Lisp error:

```lisp
((status . ok)
 (stamp
   (document-id . "k7m2qx")
   (epoch . "db-7d04")
   (change-seq . 418)
   (context-seq . 23))
 (value . ...)
 (diagnostics . ()))
```

`stamp` identifies the observed drawing generation and state. `diagnostics` contains structured warnings that do not invalidate the returned value.

`acadctl:assert` returns `T` or raises. `acadctl:require` returns an accepted result's `value` or raises. `acadctl:table` returns a string or raises.

| Status | Meaning |
| --- | --- |
| `ok` | The advertised provider completed the request. |
| `absent` | The provider covers the fact, but no value is present. |
| `unsupported` | The provider, platform, or object class does not provide the operation. |
| `unavailable` | The provider supports the operation, but current state or resources prevent it. |
| `opaque` | Data exists, but the provider cannot interpret it safely. |
| `failed` | The provider attempted the operation and failed. |
| `skipped` | Policy or an earlier item prevented the attempt. |
| `partial` | Only the identified subset is trustworthy. |
| `stale` | A reference, cursor, context, or repair no longer matches its recorded state. |
| `conflict` | A live target no longer satisfies an explicit precondition. |
| `not-applicable` | The operation has no meaning for the target. |

A valid empty collection is `status=ok` with `items=nil` and `returned=0`. Bare `nil` never represents a failed lookup, unavailable context, unsupported provider, or partial result.

An optional fact carries its own status:

```lisp
((name . extents)
 (status . unavailable)
 (error
   (code . geometry-not-regenerated)
   (retryable . T)))
```

### Errors

A structured error contains stable machine fields and a non-contract message:

```lisp
((code . stale-reference)
 (operation . acadctl:set-facts)
 (provider . core-entity)
 (subject . <object-ref>)
 (native-status . "eWasErased")
 (retryable . nil)
 (details . ...)
 (message . "Reference belongs to an earlier database generation"))
```

Agents branch on `code`, `operation`, `provider`, `native-status`, `retryable`, and `details`. `message` is not a contract field. Implementations may localize or revise it.

### Stamps and references

The document stamp contains:

- `document-id`: the six-character public document ID.
- `epoch`: an opaque identifier for the current `AcDbDatabase` generation.
- `change-seq`: a session-local monotonic sequence for drawing changes.
- `context-seq`: a session-local monotonic sequence for human selection and view context.

A reusable host-database object reference is:

```lisp
((kind . object-ref)
 (database
   (document-id . "k7m2qx")
   (epoch . "db-7d04")
   (scope . host))
 (handle . "1A2B")
 (class-hint . "AcDbLine"))
```

The uppercase handle identifies an object only within the recorded database epoch. `class-hint` helps diagnostics but never overrides the resolved runtime class. A helper may return entity names and native pointers for use during the current execution, but they are not reusable identity.

A loaded-xref reference has `scope=xref` plus a validated xref-definition chain and provider epoch. It is read-only through the host document. To change the external drawing, the agent opens or targets that drawing separately.

A subentity reference records its host object, full nested path, subentity type, available index or graphics marker, `change-seq`, `context-seq`, and `stability=transient`. A graphics marker is not reusable object identity.

`acadctl:locator` creates a longer-lived locator from a drawing fingerprint, handle, class witness, and optional caller-managed UUID witness. Resolution verifies every witness. A locator does not claim that handles survive insertion, binding, cloning, file replacement, or unrelated drawing copies.

Reference rules:

- Database replacement changes `epoch` even when `document-id` stays the same.
- Save and Save As preserve references when the live database generation stays the same.
- Erased objects resolve with explicit erased state when AutoCAD can still open them.
- Purged objects resolve as `absent`. References from an earlier epoch resolve as `stale`.
- `change-seq` detects conflicting state but is not part of object identity.
- Cross-file business identity requires an explicit UUID or another project schema.

### Pages and ordering

Collections return a page inside the result value:

```lisp
((items . (...))
 (page
   (order . ((name-casefold ascending) (name-raw ascending) (handle ascending)))
   (limit . 200)
   (returned . 200)
   (has-next . T)
   (next . "opaque-cursor")
   (total (status . unavailable))))
```

Each cursor records the document ID, epoch, change sequence, provider, root or relation, ordering, and request options. Any relevant drawing change makes it `stale`. A cursor never retains an ObjectARX object, iterator, document lock, or transaction.

Default ordering is:

- Table and dictionary records order by Unicode case-folded name, then raw name. Numeric handle breaks remaining ties.
- Entities order by owner or container key, then numeric handle.
- Dictionary entries order by raw key, then numeric handle when the key does not decide the order.
- AcRx properties order by invariant internal name.
- Dependencies order by kind, normalized path, then source handle.
- Human selection uses deterministic reference order and retains the original `observed-index`.

Mutation results preserve input order and include `input-index`. Duplicate targets are an error unless the caller supplies an explicit deduplication policy.

### Coordinates and display values

Geometry uses tagged values when a coordinate frame matters:

```lisp
((type . point3)
 (space . wcs)
 (units . drawing)
 (value 12.5 8.0 0.0))
```

Supported frames are `wcs`, `ucs`, `dcs`, `psdcs`, and `ocs`. OCS values include an owning entity or normal. Angles are radians. Positive rotation follows the right-hand rule around the supplied axis. Lengths use drawing units unless tagged otherwise.

A general transform is a 4 by 4 row-major affine matrix acting on a column vector `[x y z 1]`. Translation uses drawing units. `move`, `rotate`, `scale`, and `mirror` cover common operations without requiring matrix construction.

Computational values use invariant identifiers and exact stored values. Localized labels and formatted values appear separately as `display-name` and `display-value`. Functions never accept a localized label as an invariant property key.

## Public API

Every optional `opts` argument is an alist. Functions do not guess an option from the shape of another argument. Functions that accept several subjects return one indexed result row per subject.

### Discover the API and current document

| Function | Value |
| --- | --- |
| `(acadctl:help [symbol])` | Public-function page or one documentation record. |
| `(acadctl:schema symbol)` | Arguments, options, enums, result shape, statuses, effects, cost, and providers. |
| `(acadctl:examples [symbol] [opts])` | Offline runnable example records. |
| `(acadctl:capabilities [topic] [opts])` | Provider coverage for the current platform and document. |
| `(acadctl:document)` | Identity, epoch, state, units, active space, layout, UCS, and database roots. |
| `(acadctl:stamp)` | Current document, epoch, drawing-change, and human-context stamp. |

`acadctl:help`, `acadctl:schema`, `acadctl:examples`, and `acadctl:capabilities` are part of the runtime API. An agent must discover each call contract and returned record without searching source code or internet documentation. The metadata also states side effects, destructive requirements, cost, platform support, and loaded object-enabler requirements.

### Read human context

| Function | Value |
| --- | --- |
| `(acadctl:context [opts])` | One admission-time snapshot of active state, selection, subselection, grips coverage, and semantic view. |
| `(acadctl:selection [opts])` | Page of normalized selected-object records. |
| `(acadctl:subselection [opts])` | Page of nested and subentity paths. |
| `(acadctl:view)` | Semantic layout, viewport, camera, target, direction, twist, perspective, visual style, clipping, UCS, and device rectangle. |

`context` records both `human-active-at-admission` and `execution-current`. Temporary document activation for `eval` or `exec` cannot create a false human context.

For a human-active document, an empty selection returns `status=ok` with no items. For an inactive target, selection, subselection, and live view return `unavailable`. Capture failure returns `failed`, never an empty selection. The API may return a last-observed context only with `status=stale`.

Each selection item contains `observed-index`, an object reference, selection method when available, nested path when available, and provider coverage for grip or subentity detail. Exact hot-grip indices remain `unsupported` until live tests prove a documented observer.

Selection capture must happen at native admission before temporary document activation and before the execution undo group opens. `acedGetCurrentSelectionSet` cannot satisfy this contract because Autodesk documents that it removes PickFirst highlighting and grips. The candidate path registers a command context with the required PickFirst and redraw flags, calls `acedSSGetFirst`, then reads `acedSSNameXEx` and full subentity paths. The API exposes this path only after live macOS and Windows tests prove that membership, order, selection provenance, subentity paths, highlighting, and grips remain unchanged. See Autodesk's [`acedGetCurrentSelectionSet` contract](https://help.autodesk.com/view/OARX/2026/ENU/?guid=OARX-RefGuide-acedGetCurrentSelectionSet_AcDbObjectIdArray_).

### Resolve and traverse drawing data

| Function | Value |
| --- | --- |
| `(acadctl:refs source [opts])` | Stamped references normalized from enames, handles, picksets, context items, or existing refs. |
| `(acadctl:resolve designators [opts])` | Per-input resolution status and current-execution object value. |
| `(acadctl:lookup kind names [opts])` | Records such as layers, blocks, styles, layouts, and groups. |
| `(acadctl:locator refs [opts])` | Verified longer-lived locators where available. |
| `(acadctl:roots [opts])` | Supported database roots and provider coverage. |
| `(acadctl:relations refs [opts])` | Advertised incoming and outgoing relation types. |
| `(acadctl:related refs edge [opts])` | Page reached through one advertised typed relation. |
| `(acadctl:children containers edge [opts])` | Containment-only convenience with a mandatory edge. |
| `(acadctl:entities containers [opts])` | Convenience for an advertised `entities` containment edge. |

There is no `acadctl:objects`. ObjectARX exposes documented roots and concrete iterators, but no public whole-database object iterator. The standard library traverses supported symbol tables, dictionaries, block records, and manager-owned collections. The library reports unsupported custom ownership as a coverage gap rather than silently omitting it. Autodesk's [object ownership documentation](https://help.autodesk.com/cloudhelp/2025/FRA/OARX-DevGuide/files/GUID-F2E1C95A-E3D4-497B-8695-1C0E35006B7E.htm) permits custom ownership relationships, so the plan cannot call a scan of known roots exhaustive for unknown classes.

A relation row has a typed edge and explicit endpoints:

```lisp
((edge . block-entity)
 (from . <object-ref>)
 (to . <object-ref>)
 (key (status . not-applicable))
 (index . 17)
 (order . native))
```

Examples of fixed relation types are `table-record`, `dictionary-entry`, `block-entity`, `block-reference`, `attribute`, `extension-dictionary`, `group-member`, `layout-viewport`, `xref-child`, and `owner`. Unsupported container classes return `unsupported`, not an empty page.

### Inspect objects and providers

| Function | Value |
| --- | --- |
| `(acadctl:object-info refs [opts])` | Identity, lifecycle state, access, class, owner, container, and provider coverage. |
| `(acadctl:facts refs fields [opts])` | Batch normalized facts with one status per field. |
| `(acadctl:properties refs names-or-all [opts])` | AcRx property metadata and values from declared query contexts. |
| `(acadctl:raw-dxf refs [opts])` | Ordered raw DXF data with unsupported and opaque values identified. |
| `(acadctl:xdata refs apps-or-all [opts])` | Registered-application XData and typed raw values. |
| `(acadctl:extension-data refs [opts])` | Extension dictionary entries and paged Xrecord data. |
| `(acadctl:dependencies [opts])` | Dependency rows plus collector coverage. |

`object-info` contains fixed common fields. Optional facts such as extents, layer, layout, display state, and proxy metadata carry their own status.

`facts` uses an advertised normalized vocabulary. Initial common fields include class, DXF name, owner, container, space, layout, layer, color source and value, linetype source and value, lineweight, transparency, visibility, extents, style, block definition, and lifecycle state. Class-specific providers add fields through `acadctl:capabilities` and `acadctl:schema`.

`properties` covers declared AcRx default and promoting query contexts. Each row contains invariant path, provider, native type, normalized type, read-only state, retrieval status, typed value when representable, and separate display label, category, and display value. It does not claim Properties Palette or Windows-only OPM parity. Custom getters may be expensive or unsafe, so provider and cost metadata must identify opt-in calls.

`raw-dxf`, XData, and extension data are storage-level escape hatches. They do not claim to explain every custom object's semantics.

### Use domain catalogs

These functions are agent-facing conveniences over roots, typed relations, normalized facts, and provider operations:

```lisp
(acadctl:layers [opts])
(acadctl:blocks [opts])
(acadctl:block-references blocks [opts])
(acadctl:attributes inserts [opts])
(acadctl:dynamic-properties inserts names-or-all [opts])
(acadctl:styles kind [opts])
(acadctl:layouts [opts])
(acadctl:viewports layouts [opts])
(acadctl:groups [opts])
(acadctl:xrefs [opts])
(acadctl:xref-graph [opts])
```

`styles` accepts a fixed discoverable enum: `text`, `dimension`, `multileader`, `table`, `linetype`, `plot-settings`, `plot-style`, `view`, `ucs`, `visual`, and `material`. A provider reports unavailable or unsupported kinds through capabilities.

Bundle catalog functions in Lisp over a smaller native bulk-fact kernel. Their public names spare agents from rebuilding table and dictionary walks, attribute loops, class tests, and block-reference searches.

### Change normalized and stored data

```lisp
(acadctl:set-facts edits [opts])
(acadctl:set-properties edits [opts])
(acadctl:patch-dxf edits [opts])
(acadctl:set-xdata edits [opts])
(acadctl:remove-xdata edits [opts])
(acadctl:set-xrecord edits [opts])
(acadctl:remove-extension-entry edits [opts])
```

An edit has a target, optional expected facts, and requested changes:

```lisp
((ref . <object-ref>)
 (expect
   ((layer . "A-WALL-OLD")
    (color-source . explicit)))
 (changes
   ((layer . "A-WALL")
    (color-source . by-layer))))
```

`set-facts` changes only advertised normalized fields. `set-properties` uses invariant AcRx names. `patch-dxf` rejects identity, ownership, object type, subclass structure, and other dangerous group codes unless a class-specific schema allows them.

### Create, copy, and transform

```lisp
(acadctl:create kind specs [opts])
(acadctl:copy refs destination [opts])
(acadctl:clone refs destination [opts])
(acadctl:import-definitions source kind names [opts])

(acadctl:convert geometry to-frame [opts])
(acadctl:transform refs transform [opts])
(acadctl:move refs vector [opts])
(acadctl:rotate refs center axis radians [opts])
(acadctl:scale refs base factors [opts])
(acadctl:mirror refs plane [opts])

(acadctl:erase refs [opts])
(acadctl:restore refs [opts])
(acadctl:rename edits [opts])
(acadctl:rehome refs destination [opts])
(acadctl:ensure kind specs [opts])
(acadctl:explode refs [opts])
```

`create` uses a fixed tagged schema for every advertised kind. Initial kinds should cover line, arc, circle, polyline, hatch, text, mtext, dimension, leader, block insert, layer, and other domain objects as their providers become available.

`copy` duplicates objects within the current database and explicit owner. `clone` uses native clone semantics and returns the complete old-to-new mapping. `import-definitions` reads an explicit source and writes only the current target document. It requires duplicate-name and unit policies.

`rehome` may clone and erase rather than mutate ownership. Its result reports every replacement reference. `restore` is available only while an erased object remains resolvable in the same epoch. `explode` requires explicit destructive permission.

### Change blocks, collections, layouts, and xrefs

```lisp
(acadctl:block-create name refs base-point [opts])
(acadctl:block-insert block specs [opts])
(acadctl:set-attributes edits [opts])
(acadctl:set-dynamic-properties edits [opts])
(acadctl:sync-attributes blocks [opts])

(acadctl:group-create name refs [opts])
(acadctl:group-add edits [opts])
(acadctl:group-remove edits [opts])

(acadctl:layout-create name [opts])
(acadctl:apply-page-setup layouts setup [opts])
(acadctl:set-viewports edits [opts])
(acadctl:set-viewport-overrides edits [opts])

(acadctl:xref-attach specs [opts])
(acadctl:xref-set-path edits [opts])
(acadctl:xref-reload refs [opts])
(acadctl:xref-unload refs [opts])
(acadctl:xref-detach refs [opts])
(acadctl:xref-bind refs mode [opts])
```

Styles normally use `lookup`, `ensure`, `set-facts`, and `import-definitions`. Separate style-specific mutation functions should exist only when the native operation has distinct state or safety requirements.

`sync-attributes` refuses block references with XData or managed extension records unless the caller supplies a preservation and reconciliation policy. `xref-detach`, `xref-bind`, destructive attribute synchronization, purge, and explode require `(allow-destructive . T)`.

### Verify, diagnose, and present

```lisp
(acadctl:assert predicate code [details])
(acadctl:require result [accepted-statuses])
(acadctl:diff before after [opts])
(acadctl:check check-specs [opts])
(acadctl:repair repairs [opts])
(acadctl:database-check [opts])
(acadctl:purge-candidates kinds [opts])
(acadctl:purge refs [opts])
(acadctl:diagnostics [opts])
(acadctl:table columns rows [opts])
```

`acadctl:assert` returns `T` or raises a structured error. `acadctl:require` returns the `value` of an accepted result and raises on every other status by default. An agent that also needs the stamp retains the original result before calling `require`.

`acadctl:diff` compares Lisp values or complete inspection results with reference-aware and fact-aware normalization. Inspection result envelopes already carry stamps, so the API does not need a persistent snapshot object.

`acadctl:check` accepts fixed checker schemas rather than arbitrary native predicates. Initial checkers cover layer catalogs and naming, ByLayer policy, missing definitions, style overrides, viewport scale and locking, xref health, dependency availability, duplicate managed UUIDs, proxy inventory, invalid references, and purge candidates.

Findings identify checker, severity, subject, evidence, coverage, and repairability. Their repair descriptors name transparent public operations. Each descriptor records an epoch and change sequence alongside exact preconditions. `acadctl:repair` executes those operations. It is not a persisted plan or private mutation language.

`acadctl:database-check` is read-only by default. Any repair runs as an explicit mutating request.

`acadctl:table` returns one deterministic string from a rectangular scalar matrix. The renderer rejects inconsistent row widths and escapes newlines or control characters. It measures Unicode display width and formats numbers deterministically. It never adds terminal-dependent truncation or ellipses.

## Mutation behavior

Bulk helpers prevalidate the complete request before the first intended write. Each helper accepts global `if-epoch` and `if-change-seq` options, and each edit may include target-specific `expect` facts.

The default failure mode is `(failure . abort)`. A conflict or failure raises a structured error. The surrounding `eval` or `exec` stops, and the existing execution rollback restores the drawing to the start of its undo group.

| Failure mode | Contract |
| --- | --- |
| `abort` | Raise on the first failed operation and roll back the request. |
| `return` | Return a failure or conflict only when no helper changes remain. |
| `continue` | Continue only for provider-approved independent items and return `partial`. |

A mutation result is a change summary:

```lisp
((status . ok)
 (before
   (document-id . "k7m2qx")
   (epoch . "db-7d04")
   (change-seq . 418))
 (after
   (document-id . "k7m2qx")
   (epoch . "db-7d04")
   (change-seq . 419))
 (value
   (operation . set-facts)
   (counts
     (updated . 3)
     (unchanged . 1))
   (items
     (((input-index . 0)
       (status . ok)
       (outcome . updated)
       (before-ref . <object-ref>)
       (after-ref . <object-ref>)
       (changes . ...))))))
```

The API does not add private transactions, private undo history, persisted drawing snapshots, a generic mutation-plan format, a universal batch request, or a universal dry-run flag. One `eval` or `exec` already supplies the drawing-level undo boundary, and ordinary Lisp already supplies composition. General dry-run simulation would be false for custom setters, reactors, commands, xref loading, and proxy behavior.

Saving before risky work creates the familiar on-disk recovery point. Undo and redo follow AutoCAD's current history regardless of whether the affected step came from a user, acadctl, or another integration, while drawing rollback remains limited to drawing state and excludes Lisp variable definitions, file I/O, COM calls, saves, subprocesses, other documents, and other external effects.

## Provider boundaries

### AcRx properties

The portable property provider targets declared AcRx query contexts. Built-in `getpropertyvalue`, `setpropertyvalue`, and `dumpallproperties` remain documented escape hatches. Windows-only OPM or COM providers may be optional capabilities, but the core API never depends on them.

Some `AcRxValue` types have no typed Lisp representation. Those rows return the native type and available display value with `status=opaque` or `status=unsupported`. A third-party getter is native code and may be expensive, block, open UI, or mutate despite its nominal role. Cost and provider metadata must let the agent avoid unproved getters.

### Dependencies

`acadctl:dependencies` runs a fixed collector set for xrefs, raster images, PDF, DGN and DWF underlays, point clouds, fonts, SHX and bigfonts, linetype files, CTB and STB files, plot configurations, data links, and material textures where supported.

Each row identifies its collector, kind, source, and direct or nested relationship. It stores the declared path separately from any cached resolved path and resolution status. Filesystem existence checks are opt-in because network paths can block. A read-only dependency call does not load resources or reload xrefs. It never probes credentials, and the provider redacts data-link credentials.

`acadctl:dependencies` returns collector coverage with the data. Unknown custom dependencies are `unsupported`. The API does not claim eTransmit parity.

### Proxies and custom objects

When AutoCAD has loaded an object enabler or provider, the API exposes its declared normalized facts and AcRx properties. Without one, it returns the core runtime class, proxy metadata, operation flags, available raw DXF, XData, extension data, referenced IDs, and extents.

The API never infers vertical semantics from display geometry. It never silently explodes, flattens, or coerces a proxy. It allows erase, clone, and other mutations only when proxy flags and the operation provider permit them.

### Platform differences

Public record shapes, status symbols, option names, coordinate contracts, and error codes are the same on macOS and Windows. Provider availability, localized labels, resolved paths, fonts, object enablers, and property rows may differ. `acadctl:capabilities` reports those differences before an agent attempts a platform-specific operation.

## Acceptance workflows

The selected-object flow demonstrates the full control loop:

```lisp
(setq context-result (acadctl:context))
(setq context (acadctl:require context-result))

(acadctl:assert
  (cdr (assoc 'human-active-at-admission context))
  'inactive-human-context
  context)

(setq refs
  (acadctl:require
    (acadctl:refs (cdr (assoc 'selection context)))))

(setq before-result
  (acadctl:facts refs
    '(class layer color-source linetype-source)))
(setq before (acadctl:require before-result))
(setq before-stamp (cdr (assoc 'stamp before-result)))

(setq edits
  (mapcar
    '(lambda (ref)
       (list
         (cons 'ref ref)
         (cons 'changes
           '((color-source . by-layer)
             (linetype-source . by-layer)))))
    refs))

(acadctl:require
  (acadctl:set-facts edits
    (list
      (cons 'if-epoch (cdr (assoc 'epoch before-stamp)))
      (cons 'if-change-seq (cdr (assoc 'change-seq before-stamp))))))

(setq after-result
  (acadctl:facts refs
    '(class layer color-source linetype-source)))
(setq after (acadctl:require after-result))

(setq difference
  (acadctl:require
    (acadctl:diff before-result after-result)))
```

If `difference` shows that a selected block insert should remain ByBlock, the agent issues one corrective `set-facts` call and verifies that object again. The agent needs no selection walker, handle wrapper, property dumper, rollback helper, or reporting script.

The following workflows must also fit short compositions of public calls:

| Human request | Inspect | Change | Verify |
| --- | --- | --- | --- |
| Enforce a layer standard | `check`, `layers`, `facts` | `repair`, `ensure`, `rename`, `rehome`, `set-facts` | Repeat the same checks. |
| Update title-block data | `lookup`, `block-references`, `attributes`, `dynamic-properties` | `set-attributes`, `set-dynamic-properties`, guarded `sync-attributes` | Read attributes and extension data again. |
| Configure a sheet layout | `lookup`, `layouts`, `viewports`, `styles` | `apply-page-setup`, `set-viewports`, `set-viewport-overrides` | Read plot, viewport, scale, and lock facts again. |
| Repair a missing xref | `xref-graph`, `dependencies` | `xref-set-path`, `xref-reload` | Repeat xref and dependency inspection. |
| Create and place geometry | `ensure` definitions, inspect destination | `create`, `move`, `rotate`, `scale` | Read geometry, owner, layer, and extents. |
| Retry after a concurrent edit | Read facts and retain the stamp. | Call with `if-change-seq`, receive `conflict`, recompute. | Confirm against the new stamp. |
| Inspect a custom or proxy object | `object-info`, `properties`, `raw-dxf`, `extension-data` | Use only an advertised provider operation. | Report `opaque` or `unsupported` without coercion. |

These workflows are release acceptance tests. The agent may provide project-specific names and policies, but it must not need to define reusable traversal, conversion, mutation, or reporting helpers to complete them.

## Code ownership

| Layer | Responsibility |
| --- | --- |
| Rust | Schema and provider registries, validation, result and error records, epochs, sequences, refs, ordering, cursors, normalized values, preconditions, change summaries, checker orchestration, help generation, and table formatting. |
| C++ ObjectARX adapter | Admission-time editor context, non-consuming selection capture, native object lifetime, concrete iterators, runtime classes, proxies, AcRx get and set, clone mappings, transforms, erase and restore, layouts, xrefs, audit, purge, and reactors. |
| Bundled AutoLISP | Fixed public wrappers, catalog conveniences, paging loops, transform constructors, `ensure`, pure diff helpers, checker composition, repair selection, and examples. |
| Built-in AutoLISP | Precise existing operations such as `ssget`, `sslength`, `ssname`, `ssnamex`, `entget`, `entnext`, `entmake`, `entmakex`, `entmod`, `entupd`, `handent`, `tblsearch`, `tblnext`, dictionary functions, `getpropertyvalue`, `setpropertyvalue`, `trans`, `command-s`, and `vl-catch-all-*`. |

C++ remains narrow, but its ObjectARX calls determine AutoCAD-facing behavior and require direct tests. Rust owns public policy and data contracts. Bundled Lisp should not reproduce native traversal when a fixed bulk call can avoid repeated object opens.

Public wrappers may call one private transport:

```lisp
(acadctl:_call operation arguments options)
```

`acadctl:_call` is not public and accepts only operations registered in the Rust schema registry. The same definitions generate public wrappers, `acadctl:help`, schemas, examples, and native dispatch so signatures cannot drift.

## Build order

The order follows technical dependencies. It does not reduce the promised final surface.

1. **Schema and result bridge.** Add the private typed request and response bridge, structured errors, registry-generated public wrappers, help, schemas, examples, and capability records. Prove bounded nested alists and malformed-input handling.
2. **Epochs, sequences, and admission context.** Add database epochs, drawing and context sequences, admission-time human context, refs, resolution, locators, and stale or conflict behavior. Prove replacement, Save As, close and reopen, undo, redo, erase, restore, and interactive edits.
3. **Roots, relations, paging, and raw inspection.** Add documented roots, typed relations, deterministic cursors, object info, raw DXF, XData, extension data, and proxy metadata. No catalog becomes public before its provider coverage is queryable.
4. **Normalized facts and AcRx.** Add invariant fact schemas and declared AcRx contexts. Verify native objects, loaded object enablers, custom classes, opaque values, and getter failures on macOS and Windows.
5. **Bulk mutation.** Add complete prevalidation, per-edit expectations, failure modes, change summaries, and controlled reversal. Inject failures at every native boundary and prove the reported drawing outcome.
6. **Domain providers.** Add layers, blocks, attributes, the `acadctl:dynamic-properties` provider, styles, layouts, viewports, groups, xrefs, dependencies, clone and import, geometry creation and transforms, purge, and database checks.
7. **Checks, repairs, diff, and tables.** Add fixed checker schemas, transparent repair descriptors, deterministic findings, pure diff, and table formatting.
8. **Cross-platform and scale qualification.** Run live macOS and Windows fixtures with large drawings, large selections, deep block nesting, loaded and unloaded xrefs, Unicode names, network paths, custom objects, proxies, and rollback failures.

## Release gates

The standard library is not release-ready until live AutoCAD proves:

- Reading human selection, subselection, and grip coverage does not consume or alter the selection, highlighting, or grips.
- An inactive target never reports unavailable human context as an empty active selection.
- Every database replacement invalidates old references, locators when their witnesses fail, and cursors.
- `change-seq` observes acadctl changes, built-in Lisp and commands, interactive edits, undo, and redo before a later helper accepts a precondition as fresh.
- Page ordering repeats under an unchanged stamp, and stale cursors fail explicitly.
- WCS, UCS, OCS, DCS, and PSDCS conversions match AutoCAD.
- Custom and proxy provider calls cannot crash the host or silently coerce unsupported values.
- Dependency results identify every collector that ran and every collector that was unavailable.
- Inspection performs no intentional database or editor mutation.
- Normal save, undo, redo, and execution rollback retain the behavior specified in [plan.md](plan.md).
- Public schemas and status enums match on macOS and Windows.

Live measurements must set paging and value limits before they become fixed contracts. The starting proposal is 200 items per page, 2,000 items as the caller-selectable page maximum, a 1 MiB default value budget per helper call, and a 4 MiB hard value maximum. One oversized field returns `status=partial` with a `value-truncated` diagnostic rather than hanging or silently disappearing.

No native object, iterator, document lock, or transaction may survive a helper return. Rust may cache only lightweight keys or handles under a bounded memory limit, epoch, change sequence, and expiry. Filesystem checks and expensive custom getters are opt-in. There is no general runtime timeout because AutoCAD cannot safely preempt arbitrary native and custom code.

## Decisions that remain fixed

- Keep ordinary AutoLISP as the composition language and long-tail escape hatch.
- Keep structured Lisp values as the computational result.
- Keep one deterministic table renderer that returns a string.
- Keep ordinary AutoCAD save, undo, redo, and one undo group per nonempty execution.
- Do not add screenshots or pixel inspection to this library.
- Do not add JSON, JSONL, TSV, CSV, or output-format switches without a demonstrated consumer.
- Do not add GraphQL, a public generic `inspect` request, or another query language.
- Do not add `acadctl:objects` or claim unknown custom ownership is exhaustively enumerable.
- Do not claim Properties Palette, OPM, eTransmit, or unknown dependency parity.
- Do not add private history, persistent drawing snapshots, general mutation plans, or a universal dry run.
- Do not treat handles, selection-set numbers, graphics markers, VLA objects, or native pointers as cross-generation identity.

The design is complete when an agent can finish every acceptance workflow through discoverable public operations and inspect the exact facts it changed. It must distinguish every unsupported or uncertain case, then recover through ordinary AutoCAD save and history behavior without defining a reusable utility library of its own.
