#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include "adscodes.h"
#include "accmd.h"
#include "acedCmdNF.h"
#include "acedads.h"
#include "acdocman.h"
#include "aced.h"
#include "acestext.h"
#include "acutads.h"
#include "dbhandle.h"
#include "dbmain.h"
#include "rxregsvc.h"
#include <algorithm>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <memory>
#include <optional>
#include <syslog.h>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);
int acdbSetDbmod(AcDbDatabase *database, int value);
extern "C" int acadctl_wake_native_actions();

namespace {

std::atomic<std::uint32_t> nativeActionCallbacksOutstanding{0};
std::atomic<bool> acceptNativeActionWakes{true};
const ACHAR kHistoryCommandGroup[] = ACRX_T("ACADCTL_INTERNAL");
const ACHAR kHistoryCommandName[] = ACRX_T("ACADCTL_INTERNAL_HISTORY");
const ACHAR kHistoryCommandInvocation[] =
    ACRX_T("ACADCTL_INTERNAL_HISTORY\n");

AcString nativeString(const rust::String &value) {
  return AcString(value.data(), AcString::Utf8,
                  static_cast<Adesk::UInt32>(value.size()));
}

struct BridgeProtocolText {
  BridgeProtocolText() : BridgeProtocolText(acadctl::native_bridge_protocol()) {}

  explicit BridgeProtocolText(acadctl::NativeBridgeProtocol protocol)
      : executionDriverExpression(
            nativeString(protocol.execution_driver_expression)),
        executionDriverInvocation(
            nativeString(protocol.execution_driver_invocation)),
        valueEventFunction(nativeString(protocol.value_event_function)),
        advanceExecutionFunction(
            nativeString(protocol.advance_execution_function)),
        sourceSymbol(nativeString(protocol.source_symbol)),
        stagedFormSymbol(nativeString(protocol.staged_form_symbol)),
        statusSymbol(nativeString(protocol.status_symbol)),
        errorSymbol(nativeString(protocol.error_symbol)),
        errnoSymbol(nativeString(protocol.errno_symbol)),
        valueSymbol(nativeString(protocol.value_symbol)),
        pendingStatus(nativeString(protocol.pending_status)),
        valueChunkCaptureUnits(protocol.value_chunk_capture_units) {}

  AcString executionDriverExpression;
  AcString executionDriverInvocation;
  AcString valueEventFunction;
  AcString advanceExecutionFunction;
  AcString sourceSymbol;
  AcString stagedFormSymbol;
  AcString statusSymbol;
  AcString errorSymbol;
  AcString errnoSymbol;
  AcString valueSymbol;
  AcString pendingStatus;
  std::size_t valueChunkCaptureUnits;
};

const BridgeProtocolText &bridgeProtocol() {
  static const BridgeProtocolText protocol;

  return protocol;
}

class NativeActionCallbackLease final {
public:
  ~NativeActionCallbackLease() {
    nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  }
};

class DatabaseReactor final : public AcDbDatabaseReactor {
public:
  void objectAppended(const AcDbDatabase *, const AcDbObject *) override {
    markChanged();
  }

  void objectUnAppended(const AcDbDatabase *, const AcDbObject *) override {
    markChanged();
  }

  void objectReAppended(const AcDbDatabase *, const AcDbObject *) override {
    markChanged();
  }

  void objectOpenedForModify(const AcDbDatabase *,
                             const AcDbObject *) override {
    markChanged();
  }

  void objectModified(const AcDbDatabase *, const AcDbObject *) override {
    markChanged();
  }

  void objectErased(const AcDbDatabase *, const AcDbObject *, bool) override {
    markChanged();
  }

  void headerSysVarWillChange(const AcDbDatabase *, const ACHAR *) override {
    markChanged();
  }

  void headerSysVarChanged(const AcDbDatabase *, const ACHAR *, bool) override {
    markChanged();
  }

  void proxyResurrectionCompleted(const AcDbDatabase *, const ACHAR *,
                                  AcDbObjectIdArray &) override {
    markChanged();
  }

  void goodbye(const AcDbDatabase *) override {
    changed_.store(1, std::memory_order_release);
    databaseGone_.store(true, std::memory_order_release);
  }

  bool takeChanged() {
    return changed_.exchange(0, std::memory_order_relaxed) != 0;
  }

  bool databaseGone() const {
    return databaseGone_.load(std::memory_order_acquire);
  }

private:
  void markChanged() {
    changed_.store(1, std::memory_order_relaxed);
  }

  std::atomic<std::uint32_t> changed_{0};
  std::atomic<bool> databaseGone_{false};
};

static_assert(std::atomic<std::uint32_t>::is_always_lock_free);

struct DocumentSubscription {
  AcApDocument *document;
  AcDbDatabase *database;
  AcDbDatabase *retiredDatabase;
  bool lispFunctionsDefined;
  DatabaseReactor *databaseReactor;
};

acadctl::NativeActionResult result(acadctl::NativeActionResultKind kind);

acadctl::NativeActionResult nativeFailure(
    acadctl::NativeActionResultKind kind, Acad::ErrorStatus status);

acadctl::NativeActionResult bridgeFailure(
    acadctl::NativeActionResultKind kind, int status);

int acadctlAdvanceExecution() noexcept;
int undefineLispFunctions();

enum class UndoGroupState { Inactive, Active, Unknown };

class ObjectArxBridge {
public:
  ObjectArxBridge();

  ~ObjectArxBridge();

  Acad::ErrorStatus start();

  bool stop();

  void processNextAction();

  void setLispFunctionsDefined(AcApDocument *document, bool defined);

private:
  AcApDocument *document(std::size_t token);

  bool lispFunctionsDefined(AcApDocument *document) const;

  acadctl::NativeActionResult open(const rust::String &path);

  acadctl::NativeActionResult save(AcApDocument *document);

  acadctl::NativeActionResult close(AcApDocument *document, bool discard);

  bool queueDocumentContextDispatch(const acadctl::NativeAction &action,
                                    acadctl::NativeActionResult &failure);

  void scheduleDocumentContextFinalizer();

  void scheduleExecutionDispatchFinalizer();

  void queuedHistoryCommandTerminated(const ACHAR *commandName);

  void queuedExecutionDriverStarted(const ACHAR *firstLine);

  void queuedExecutionDriverTerminated(bool cancelled);

  void recoverCancelledExecutionDriver();

  void failExecutionDriver();

  void finishAdvanceCallback(bool evaluateStagedForm);

  struct DocumentContextDispatch {
    enum class Phase { Queued, Running, Finalizing };
    enum class Kind { Undo, Redo, ExecutionDriver };
    enum class StagedFormKind { None, Evaluator, ValueVisitor };

    std::uint64_t jobId;
    std::size_t documentToken;
    std::size_t databaseToken;
    std::size_t previousActiveToken;
    Kind kind;
    bool restorePreviousActive;
    acadctl::NativeActionResult dispatchResult;
    Phase phase;
    UndoGroupState undoGroup = UndoGroupState::Inactive;
    bool executionUndoGroupMayHaveStarted = false;
    bool formHandedOff = false;
    StagedFormKind stagedFormKind = StagedFormKind::None;
    bool retainValue = false;
    bool bridgeSymbolsMayBeRetained = false;
    bool driverExitReady = false;
    bool driverStarted = false;
    bool driverEnded = false;
    bool advanceCallbackActive = false;
    bool terminalCleanupFailed = false;
    std::uint32_t lispDepth = 0;
    std::optional<rust::Box<acadctl::NativeValueWriter>> valueWriter;
  };

  static acadctl::NativeExecutionFinalizationObservation
  finalizationObservation(const DocumentContextDispatch &dispatch);

  static void runQueuedHistoryCommand();

  static void finalizeDocumentContextDispatch(void *data);

  friend int acadctlAdvanceExecution() noexcept;

  void publishDocumentSnapshot();

  void refreshDocumentSnapshot();

  void refreshDocumentSnapshotIfStale();

  void drainDatabaseChanges();

  void drainDatabaseChanges(DocumentSubscription &subscription);

  void eraseDatabaseReactor(DatabaseReactor *reactor);

  void detachDatabaseReactor(DocumentSubscription &subscription);

  void refreshSubscription(DocumentSubscription &subscription);

  void databaseWillBeDestroyed(AcDbDatabase *database);

  void actionTargetWillBeDestroyed(AcApDocument *document);

  void subscribe(AcApDocument *document);

  void unsubscribe(AcApDocument *document);

  class DocumentReactor final : public AcApDocManagerReactor {
  public:
    explicit DocumentReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

    void documentCreated(AcApDocument *document) override {
      bridge_.subscribe(document);
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentToBeDestroyed(AcApDocument *document) override {
      bridge_.actionTargetWillBeDestroyed(document);
      bridge_.unsubscribe(document);
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentTitleUpdated(AcApDocument *) override {
      bridge_.refreshDocumentSnapshot();
    }

    void documentBecameCurrent(AcApDocument *) override {
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentActivated(AcApDocument *) override {
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

  private:
    ObjectArxBridge &bridge_;
  };

  class EditorReactor final : public AcEditorReactor {
  public:
    explicit EditorReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

    void lispWillStart(const ACHAR *firstLine) override {
      bridge_.queuedExecutionDriverStarted(firstLine);
    }

    void commandEnded(const ACHAR *) override {
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandCancelled(const ACHAR *commandName) override {
      bridge_.queuedHistoryCommandTerminated(commandName);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandFailed(const ACHAR *commandName) override {
      bridge_.queuedHistoryCommandTerminated(commandName);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void lispEnded() override {
      bridge_.queuedExecutionDriverTerminated(false);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void lispCancelled() override {
      bridge_.queuedExecutionDriverTerminated(true);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void saveComplete(AcDbDatabase *, const ACHAR *) override {
      bridge_.refreshDocumentSnapshot();
    }

    void abortSave(AcDbDatabase *) override { bridge_.refreshDocumentSnapshot(); }

    void curDocOpenUpgraded(AcDbDatabase *, const CAdUiPathname &) override {
      bridge_.refreshDocumentSnapshot();
    }

    void curDocOpenDowngraded(AcDbDatabase *, const CAdUiPathname &) override {
      bridge_.refreshDocumentSnapshot();
    }

    void databaseToBeDestroyed(AcDbDatabase *database) override {
      bridge_.databaseWillBeDestroyed(database);
    }

  private:
    ObjectArxBridge &bridge_;
  };

  std::vector<DocumentSubscription> subscriptions_;
  std::vector<std::unique_ptr<DatabaseReactor>> databaseReactors_;
  DocumentReactor documentReactor_;
  EditorReactor editorReactor_;
  std::atomic<bool> documentSnapshotStale_{false};
  bool databaseReactorOwnershipUncertain_ = false;
  bool historyCommandRegistered_ = false;
  std::optional<DocumentContextDispatch> documentContextDispatch_;

  static ObjectArxBridge *commandBridge_;
};

acadctl::NativeActionResult result(acadctl::NativeActionResultKind kind) {
  return {kind, 0, rust::String()};
}

acadctl::NativeActionResult nativeFailure(
    acadctl::NativeActionResultKind kind, Acad::ErrorStatus status) {
  const AcString detail(acadErrorStatusText(status));

  return {kind, static_cast<std::int32_t>(status), rust::String(detail.utf8Ptr())};
}

acadctl::NativeActionResult bridgeFailure(
    acadctl::NativeActionResultKind kind, int status) {
  return {kind, status, rust::String()};
}

acadctl::NativeExecutionStepResult stepSuccess() {
  return {acadctl::NativeExecutionStepResultKind::Success, 0, 0,
          rust::String(), 0};
}

acadctl::NativeExecutionStepResult stepNativeFailure(int status) {
  return {acadctl::NativeExecutionStepResultKind::NativeError, status, 0,
          rust::String(), 0};
}

struct ResbufDeleter {
  void operator()(resbuf *value) const {
    if (value) {
      acutRelRb(value);
    }
  }
};

using ResbufPtr = std::unique_ptr<resbuf, ResbufDeleter>;

constexpr int kEvalValueEventFunctionCode = 1;
constexpr int kAdvanceExecutionFunctionCode = 2;

thread_local acadctl::NativeValueWriter *activeValueWriter = nullptr;

struct BoundedNativeText {
  rust::String text;
  bool truncated;
};

std::size_t boundedWideChunkLength(const ACHAR *text);
BoundedNativeText boundedDiagnostic(const ACHAR *text);
int integerValue(const resbuf *value);
int clearSymbol(const ACHAR *name);
bool matchesExecutionContext(AcApDocument *document,
                             std::size_t databaseToken,
                             AcApDocument *expectedActive);

void finishValueWriter(
    std::optional<rust::Box<acadctl::NativeValueWriter>> &retainedWriter,
    bool invalidate) {
  activeValueWriter = nullptr;

  if (!retainedWriter) {
    return;
  }

  if (invalidate) {
    acadctl::invalidate_value_writer(**retainedWriter);
  }

  rust::Box<acadctl::NativeValueWriter> writer =
      std::move(*retainedWriter);
  retainedWriter.reset();
  acadctl::finish_value_writer(std::move(writer));
}

std::size_t boundedWideChunkLength(const ACHAR *text) {
  const std::size_t captureUnits = bridgeProtocol().valueChunkCaptureUnits;
  std::size_t length = 0;

  while (length < captureUnits && text[length] != 0) {
    ++length;
  }

  if constexpr (sizeof(ACHAR) == 2) {
    if (length == captureUnits && text[length] != 0) {
      const auto last = static_cast<std::uint32_t>(text[length - 1]);
      const auto next = static_cast<std::uint32_t>(text[length]);

      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 &&
          next <= 0xdfff) {
        --length;
      }
    }
  }

  return length;
}

BoundedNativeText boundedDiagnostic(const ACHAR *text) {
  if (!text) {
    return {rust::String(), false};
  }

  const std::size_t captureUnits = acadctl::native_diagnostic_capture_units();

  if (captureUnits < 2) {
    return {rust::String(), false};
  }

  std::size_t length = 0;

  while (length < captureUnits && text[length] != 0) {
    ++length;
  }

  const bool truncated = length == captureUnits && text[length] != 0;

  if constexpr (sizeof(ACHAR) == 2) {
    if (truncated) {
      const auto last = static_cast<std::uint32_t>(text[length - 1]);
      const auto next = static_cast<std::uint32_t>(text[length]);

      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 &&
          next <= 0xdfff) {
        --length;
      }
    }
  }

  const AcString captured(text, static_cast<Adesk::UInt32>(length));
  const char *utf8 = captured.utf8Ptr();

  if (!utf8) {
    return {rust::String(), truncated};
  }

  return {rust::String(utf8), truncated};
}

acadctl::NativeLispValueEvent
lispValueEvent(int code, acadctl::NativeLispPayloadKind payloadKind) {
  return {code, payloadKind, 0, 0.0, false};
}

bool writeLispValueEvent(acadctl::NativeValueWriter &writer,
                         acadctl::NativeLispValueEvent event,
                         rust::Str text = rust::Str()) {
  return acadctl::write_lisp_value_event(writer, event, text) ==
         acadctl::NativeValueWriteResult::Continue;
}

bool writeInvalidLispValueEvent(acadctl::NativeValueWriter &writer) {
  return writeLispValueEvent(
      writer,
      lispValueEvent(0, acadctl::NativeLispPayloadKind::Invalid));
}

bool writePrivateStringPayload(acadctl::NativeValueWriter &writer, int code,
                               const ACHAR *text) {
  if (!text) {
    return writeInvalidLispValueEvent(writer);
  }

  const std::size_t length = boundedWideChunkLength(text);

  if (text[length] != 0) {
    return writeInvalidLispValueEvent(writer);
  }

  const AcString value(text, static_cast<Adesk::UInt32>(length));
  const char *utf8 = value.utf8Ptr();

  if (!utf8) {
    return writeInvalidLispValueEvent(writer);
  }

  acadctl::NativeLispValueEvent event =
      lispValueEvent(code, acadctl::NativeLispPayloadKind::String);
  event.has_text = true;

  return writeLispValueEvent(writer, event,
                             rust::Str(utf8, std::strlen(utf8)));
}

bool writePrivateEntityPayload(acadctl::NativeValueWriter &writer, int code,
                               const ads_name name) {
  acadctl::NativeLispValueEvent event =
      lispValueEvent(code, acadctl::NativeLispPayloadKind::Entity);
  AcDbObjectId objectId;

  if (acdbGetObjectId(objectId, name) != Acad::eOk || objectId.isNull()) {
    return writeLispValueEvent(writer, event);
  }

  ACHAR handleText[AcDbHandle::kStrSiz]{};

  if (!objectId.handle().getIntoAsciiBuffer(handleText)) {
    return writeLispValueEvent(writer, event);
  }

  const AcString handle(handleText);
  const char *utf8 = handle.utf8Ptr();

  if (!utf8) {
    return writeInvalidLispValueEvent(writer);
  }

  event.has_text = true;

  return writeLispValueEvent(writer, event,
                             rust::Str(utf8, std::strlen(utf8)));
}

bool writePrivateValueEvent(acadctl::NativeValueWriter &writer,
                            const resbuf *arguments) {
  if (!arguments || !arguments->rbnext || arguments->rbnext->rbnext ||
      (arguments->restype != RTSHORT && arguments->restype != RTLONG)) {
    return writeInvalidLispValueEvent(writer);
  }

  const int code = integerValue(arguments);
  const resbuf *payload = arguments->rbnext;
  acadctl::NativeLispValueEvent event =
      lispValueEvent(code, acadctl::NativeLispPayloadKind::Invalid);

  switch (payload->restype) {
  case RTNIL:
    event.payload_kind = acadctl::NativeLispPayloadKind::Nil;

    return writeLispValueEvent(writer, event);
  case RTSHORT:
  case RTLONG:
    event.payload_kind = acadctl::NativeLispPayloadKind::Integer;
    event.integer = integerValue(payload);

    return writeLispValueEvent(writer, event);
  case RTINT64:
    event.payload_kind = acadctl::NativeLispPayloadKind::Integer;
    event.integer = payload->resval.mnInt64;

    return writeLispValueEvent(writer, event);
  case RTREAL:
    event.payload_kind = acadctl::NativeLispPayloadKind::Real;
    event.real = payload->resval.rreal;

    return writeLispValueEvent(writer, event);
  case RTSTR:
    return writePrivateStringPayload(writer, code,
                                     payload->resval.rstring);
  case RTENAME:
    return writePrivateEntityPayload(writer, code,
                                     payload->resval.rlname);
  default:
    return writeLispValueEvent(writer, event);
  }
}

int acadctlEvalValueEvent() noexcept {
  try {
    bool keepGoing = false;

    if (activeValueWriter) {
      keepGoing =
          writePrivateValueEvent(*activeValueWriter, acedGetArgs());
    }

    const int returnStatus = keepGoing ? acedRetT() : acedRetNil();

    if (returnStatus != RTNORM && activeValueWriter) {
      acadctl::invalidate_value_writer(*activeValueWriter);
    }

    return returnStatus == RTNORM ? RSRSLT : RSERR;
  } catch (...) {
    if (activeValueWriter) {
      acadctl::invalidate_value_writer(*activeValueWriter);
    }

    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }
}

int defineLispFunction(const ACHAR *name, int code, int (*callback)()) {
  int status = acedDefun(name, code);

  if (status != RTNORM) {
    return status;
  }

  status = acedRegFunc(callback, code);

  if (status != RTNORM) {
    acedUndef(name, code);
  }

  return status;
}

int defineLispFunctions() {
  const BridgeProtocolText &protocol = bridgeProtocol();
  int status = defineLispFunction(protocol.valueEventFunction.kACharPtr(),
                                  kEvalValueEventFunctionCode,
                                  &acadctlEvalValueEvent);

  if (status != RTNORM) {
    undefineLispFunctions();

    return status;
  }

  status = defineLispFunction(protocol.advanceExecutionFunction.kACharPtr(),
                              kAdvanceExecutionFunctionCode,
                              &acadctlAdvanceExecution);

  if (status != RTNORM) {
    undefineLispFunctions();

    return status;
  }

  return status;
}

int undefineLispFunctions() {
  const BridgeProtocolText &protocol = bridgeProtocol();
  const int executionStatus =
      acedUndef(protocol.advanceExecutionFunction.kACharPtr(),
                kAdvanceExecutionFunctionCode);
  const int privateStatus =
      acedUndef(protocol.valueEventFunction.kACharPtr(),
                kEvalValueEventFunctionCode);
  if (executionStatus != RTNORM) {
    return executionStatus;
  }

  return privateStatus;
}

int putStringSymbol(const ACHAR *name, const AcString &text) {
  resbuf value{};
  value.restype = RTSTR;
  value.resval.rstring = const_cast<ACHAR *>(text.kACharPtr());

  return acedPutSym(name, &value);
}

int clearSymbol(const ACHAR *name) {
  resbuf value{};
  value.restype = RTNIL;

  return acedPutSym(name, &value);
}

ResbufPtr getSymbol(const ACHAR *name, int &status) {
  resbuf *value = nullptr;
  status = acedGetSym(name, &value);

  return ResbufPtr(value);
}

int integerValue(const resbuf *value) {
  if (!value) {
    return 0;
  }

  if (value->restype == RTSHORT) {
    return value->resval.rint;
  }

  if (value->restype == RTLONG) {
    return value->resval.rlong;
  }

  return 0;
}

bool getIntegerSystemVariable(const ACHAR *name, int &value, int &status) {
  resbuf result{};
  status = acedGetVar(name, &result);

  if (status != RTNORM ||
      (result.restype != RTSHORT && result.restype != RTLONG)) {
    if (status == RTNORM) {
      status = RTERROR;
    }

    return false;
  }

  value = integerValue(&result);

  return true;
}

UndoGroupState observeUndoGroup(int &status) {
  int undoControl = 0;

  if (!getIntegerSystemVariable(ACRX_T("UNDOCTL"), undoControl, status)) {
    return UndoGroupState::Unknown;
  }

  return (undoControl & 8) != 0 ? UndoGroupState::Active
                                : UndoGroupState::Inactive;
}

int clearExecutionBridgeSymbols(bool includeValue = true) {
  const BridgeProtocolText &protocol = bridgeProtocol();
  int firstFailure = RTNORM;

  for (const ACHAR *name : {protocol.sourceSymbol.kACharPtr(),
                            protocol.stagedFormSymbol.kACharPtr(),
                            protocol.statusSymbol.kACharPtr(),
                            protocol.errorSymbol.kACharPtr(),
                            protocol.errnoSymbol.kACharPtr()}) {
    const int status = clearSymbol(name);

    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }

  if (includeValue) {
    const int status = clearSymbol(protocol.valueSymbol.kACharPtr());

    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }

  return firstFailure;
}

acadctl::NativeBridgeStepResult completeEvaluationCleanup(
    acadctl::NativeBridgeCleanupPlan plan, int cleanupStatus) {
  int fallbackCleanupStatus = RTNORM;

  if (cleanupStatus != RTNORM) {
    fallbackCleanupStatus = clearExecutionBridgeSymbols();
  }

  return acadctl::complete_bridge_cleanup(
      std::move(plan), cleanupStatus == RTNORM ? 0 : cleanupStatus,
      fallbackCleanupStatus == RTNORM ? 0 : fallbackCleanupStatus);
}

acadctl::NativeBridgeStepResult
finishEvaluation(acadctl::NativeBridgeCleanupPlan plan) {
  const int cleanupStatus =
      clearExecutionBridgeSymbols(!plan.retain_value);

  return completeEvaluationCleanup(std::move(plan), cleanupStatus);
}

acadctl::NativeBridgeStepResult
finishEvaluation(acadctl::NativeExecutionStepResult result,
                 bool retainValueOnSuccess) {
  return finishEvaluation(acadctl::prepare_bridge_cleanup(
      std::move(result), retainValueOnSuccess));
}

acadctl::NativeBridgeStepResult stageEvaluation(rust::Str source,
                                                const AcString &evaluatorText,
                                                bool retainValue) {
  const BridgeProtocolText &protocol = bridgeProtocol();
  const int clearStatus = clearExecutionBridgeSymbols();

  if (clearStatus != RTNORM) {
    return completeEvaluationCleanup(
        acadctl::prepare_bridge_cleanup(stepNativeFailure(clearStatus), false),
        clearStatus);
  }

  {
    const AcString form(source.data(), AcString::Utf8,
                        static_cast<Adesk::UInt32>(source.size()));

    if (putStringSymbol(protocol.sourceSymbol.kACharPtr(), form) != RTNORM ||
        putStringSymbol(protocol.stagedFormSymbol.kACharPtr(), evaluatorText) !=
            RTNORM ||
        putStringSymbol(protocol.statusSymbol.kACharPtr(),
                        protocol.pendingStatus) != RTNORM) {
      return finishEvaluation(stepNativeFailure(RTERROR), retainValue);
    }
  }

  return {stepSuccess(), true};
}

acadctl::NativeLispObservation observeLispOutcome(int commandStatus) {
  const BridgeProtocolText &protocol = bridgeProtocol();
  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(protocol.statusSymbol.kACharPtr(), statusResult);
  const bool nilStatus =
      statusResult == RTNIL ||
      (statusResult == RTNORM && !status) ||
      (statusResult == RTNORM && status && status->restype == RTNIL);
  const acadctl::NativeLispStatusKind statusKind =
      nilStatus
          ? acadctl::NativeLispStatusKind::Nil
          : statusResult != RTNORM
                ? acadctl::NativeLispStatusKind::Unavailable
                : status && status->restype == RTT
                      ? acadctl::NativeLispStatusKind::True
                      : acadctl::NativeLispStatusKind::Other;

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(protocol.errnoSymbol.kACharPtr(), errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;

  int errorResult = RTERROR;
  ResbufPtr error = getSymbol(protocol.errorSymbol.kACharPtr(), errorResult);
  BoundedNativeText detail{rust::String(), false};
  const bool errorAvailable =
      errorResult == RTNORM && error && error->restype == RTSTR &&
      error->resval.rstring;

  if (errorAvailable) {
    detail = boundedDiagnostic(error->resval.rstring);
  }

  return {commandStatus == RTNORM ? 0 : commandStatus,
          statusKind,
          statusResult == RTNORM || nilStatus ? 0 : statusResult,
          errnoResult == RTNORM,
          lispErrnoValue,
          errorAvailable,
          std::move(detail.text),
          detail.truncated,
          RTERROR};
}

acadctl::NativeBridgeCleanupPlan collectEvaluation(bool retainValue) {
  return acadctl::interpret_lisp_observation(observeLispOutcome(RTNORM),
                                             retainValue);
}

acadctl::NativeBridgeCleanupPlan valueVisitorOutcome(int commandStatus) {
  return acadctl::interpret_lisp_observation(
      observeLispOutcome(commandStatus), false);
}

struct UndoCommandResult {
  acadctl::NativeExecutionStepResult result;
  UndoGroupState state;
};

UndoCommandResult runUndoCommand(const ACHAR *option,
                                 UndoGroupState expectedState) {
  const int commandStatus =
      acedCommandS(RTSTR, ACRX_T("_.UNDO"), RTSTR, option, RTNONE);
  int observationStatus = RTERROR;
  const UndoGroupState state = observeUndoGroup(observationStatus);

  if (commandStatus != RTNORM) {
    return {stepNativeFailure(commandStatus), state};
  }

  if (state == UndoGroupState::Unknown) {
    return {stepNativeFailure(observationStatus), state};
  }

  if (state != expectedState) {
    return {stepNativeFailure(RTERROR), state};
  }

  return {stepSuccess(), state};
}

UndoCommandResult rollbackUndoGroup(UndoGroupState state,
                                    bool executionUndoGroupMayHaveStarted) {
  if (!executionUndoGroupMayHaveStarted ||
      state == UndoGroupState::Unknown) {
    return {stepNativeFailure(RTERROR), state};
  }

  UndoCommandResult end{stepSuccess(), state};

  if (state == UndoGroupState::Active) {
    end = runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);

    if (end.state != UndoGroupState::Inactive) {
      return end;
    }
  }

  const int status = acedCommandS(RTSTR, ACRX_T("_.U"), RTNONE);
  int observationStatus = RTERROR;
  const UndoGroupState finalState = observeUndoGroup(observationStatus);

  if (status != RTNORM) {
    return {stepNativeFailure(status), finalState};
  }

  if (finalState != UndoGroupState::Inactive) {
    return {stepNativeFailure(observationStatus), finalState};
  }

  if (end.result.kind !=
      acadctl::NativeExecutionStepResultKind::Success) {
    return {std::move(end.result), finalState};
  }

  return {stepSuccess(), finalState};
}

bool matchesDatabase(AcApDocument *document, std::size_t databaseToken) {
  return document &&
         static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
             document->database())) == databaseToken;
}

bool matchesExecutionContext(AcApDocument *document,
                             std::size_t databaseToken,
                             AcApDocument *expectedActive) {
  return matchesDatabase(document, databaseToken) &&
         acDocManager->curDocument() == document &&
         acDocManager->mdiActiveDocument() == expectedActive;
}

int clearExecutionBridgeSymbolsIfSafe(AcApDocument *document,
                                      std::size_t databaseToken,
                                      AcApDocument *expectedActive,
                                      bool &bridgeSymbolsMayBeRetained) {
  if (!bridgeSymbolsMayBeRetained) {
    return RTNORM;
  }

  if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
    return RTREJ;
  }

  const int cleanupStatus = clearExecutionBridgeSymbols();

  if (cleanupStatus == RTNORM) {
    bridgeSymbolsMayBeRetained = false;
  }

  return cleanupStatus;
}

acadctl::NativeActionResult abandonLostExecutionContext(
    std::uint64_t jobId, AcApDocument *document,
    std::size_t databaseToken, AcApDocument *expectedActive,
    bool &bridgeSymbolsMayBeRetained) {
  const int cleanupStatus = clearExecutionBridgeSymbolsIfSafe(
      document, databaseToken, expectedActive, bridgeSymbolsMayBeRetained);

  if (!acadctl::abandon_execution(
          jobId,
          stepNativeFailure(cleanupStatus == RTNORM ? RTERROR : cleanupStatus))) {
    return bridgeFailure(acadctl::NativeActionResultKind::ExecutionBridgeFailed,
                         RTERROR);
  }

  return result(acadctl::NativeActionResultKind::Success);
}

void scheduleNextNativeAction() {
  if (!acadctl::try_claim_native_action_wake()) {
    return;
  }

  const int status = acadctl_wake_native_actions();

  if (status != 0) {
    acadctl::native_action_wake_failed(status);
  }
}

ObjectArxBridge::ObjectArxBridge()
    : documentReactor_(*this), editorReactor_(*this) {
  commandBridge_ = this;
}

ObjectArxBridge::~ObjectArxBridge() {
  if (commandBridge_ == this) {
    commandBridge_ = nullptr;
  }
}

ObjectArxBridge *ObjectArxBridge::commandBridge_ = nullptr;

Acad::ErrorStatus ObjectArxBridge::start() {
  (void)bridgeProtocol();
  const Acad::ErrorStatus commandStatus = acedRegCmds->addCommand(
      kHistoryCommandGroup, kHistoryCommandName,
      kHistoryCommandName,
      ACRX_CMD_MODAL | ACRX_CMD_NOHISTORY | ACRX_CMD_NO_UNDO_MARKER,
      runQueuedHistoryCommand);

  if (commandStatus != Acad::eOk) {
    return commandStatus;
  }

  historyCommandRegistered_ = true;

  acDocManager->addReactor(&documentReactor_);
  acedEditor->addReactor(&editorReactor_);

  auto iterator = acDocManager->getDocumentIterator();

  while (!iterator->done()) {
    if (AcApDocument *document = iterator->document()) {
      subscribe(document);
    }

    iterator->step();
  }

  refreshDocumentSnapshot();

  return Acad::eOk;
}

bool ObjectArxBridge::stop() {
  if (documentContextDispatch_) {
    return false;
  }

  if (historyCommandRegistered_) {
    const Acad::ErrorStatus status =
        acedRegCmds->removeGroup(kHistoryCommandGroup);

    if (status != Acad::eOk && status != Acad::eKeyNotFound) {
      return false;
    }

    historyCommandRegistered_ = false;
  }

  for (DocumentSubscription &subscription : subscriptions_) {
    detachDatabaseReactor(subscription);
  }

  subscriptions_.clear();
  databaseReactors_.erase(
      std::remove_if(databaseReactors_.begin(), databaseReactors_.end(),
                     [](const auto &uncertain) {
                       return uncertain->databaseGone();
                     }),
      databaseReactors_.end());

  if (!databaseReactors_.empty()) {
    return false;
  }

  acedEditor->removeReactor(&editorReactor_);
  acDocManager->removeReactor(&documentReactor_);

  return true;
}

void ObjectArxBridge::processNextAction() {
  drainDatabaseChanges();
  acadctl::NativeAction action = acadctl::take_native_action();

  if (action.kind == acadctl::NativeActionKind::None) {
    scheduleNextNativeAction();

    return;
  }

  acadctl::NativeActionResult actionResult =
      result(acadctl::NativeActionResultKind::Success);

  switch (action.kind) {
  case acadctl::NativeActionKind::Open:
    actionResult = open(action.path);
    break;
  case acadctl::NativeActionKind::Save:
    if (AcApDocument *target = document(action.document_token)) {
      actionResult =
          matchesDatabase(target, action.database_token)
              ? save(target)
              : result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DocumentGone);
    }

    break;
  case acadctl::NativeActionKind::Close:
    if (AcApDocument *target = document(action.document_token)) {
      actionResult =
          matchesDatabase(target, action.database_token)
              ? close(target, action.discard)
              : result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DocumentGone);
    }

    break;
  case acadctl::NativeActionKind::Undo:
  case acadctl::NativeActionKind::Redo:
  case acadctl::NativeActionKind::QueueExecutionDriver:
    if (queueDocumentContextDispatch(action, actionResult)) {
      return;
    }

    break;
  case acadctl::NativeActionKind::None:
    return;
  }

  refreshDocumentSnapshot();
  acadctl::complete_native_action(action.job_id, std::move(actionResult));
  scheduleNextNativeAction();
}

void ObjectArxBridge::setLispFunctionsDefined(AcApDocument *document,
                                              bool defined) {
  if (!document) {
    return;
  }

  if (defined) {
    subscribe(document);
  }

  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocumentSubscription &candidate) {
                     return candidate.document == document;
                   });

  if (subscription == subscriptions_.end()) {
    return;
  }

  if (defined) {
    refreshSubscription(*subscription);
  }

  subscription->lispFunctionsDefined = defined;
}

AcApDocument *ObjectArxBridge::document(std::size_t token) {
  const auto subscription = std::find_if(
      subscriptions_.begin(), subscriptions_.end(),
      [token](const DocumentSubscription &candidate) {
        return static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
                   candidate.document)) == token;
      });

  return subscription == subscriptions_.end() ? nullptr
                                              : subscription->document;
}

bool ObjectArxBridge::lispFunctionsDefined(AcApDocument *document) const {
  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocumentSubscription &candidate) {
                     return candidate.document == document;
                   });

  return subscription != subscriptions_.end() &&
         subscription->lispFunctionsDefined;
}

acadctl::NativeActionResult ObjectArxBridge::open(const rust::String &path) {
  const AcString drawingPath(path.data(), AcString::Utf8,
                             static_cast<Adesk::UInt32>(path.size()));
  AcApDocManager::DocOpenParams parameters{};
  parameters.mpwszFileName = drawingPath.kACharPtr();
  parameters.mnInitialViewType = AcApDocManager::DocOpenParams::kDefaultView;
  parameters.mnFlags = AcApDocManager::DocOpenParams::kFileNameArgIsUnicode;

  const Acad::ErrorStatus status =
      acDocManager->appContextOpenDocument(&parameters);

  return status == Acad::eOk
             ? result(acadctl::NativeActionResultKind::Success)
             : nativeFailure(acadctl::NativeActionResultKind::OpenFailed,
                             status);
}

acadctl::NativeActionResult ObjectArxBridge::save(AcApDocument *document) {
  if (!document->isNamedDrawing()) {
    return result(acadctl::NativeActionResultKind::Unnamed);
  }

  if (document->isReadOnly()) {
    return result(acadctl::NativeActionResultKind::ReadOnly);
  }

  const Acad::ErrorStatus lockStatus = acDocManager->lockDocument(
      document, AcAp::kXWrite, nullptr, nullptr, false);

  if (lockStatus != Acad::eOk) {
    return nativeFailure(acadctl::NativeActionResultKind::LockFailed,
                         lockStatus);
  }

  AcApDocument *active = acDocManager->mdiActiveDocument();
  bool changedCurrent = active != document;
  Acad::ErrorStatus status = Acad::eOk;

  if (changedCurrent) {
    status = acDocManager->setCurDocument(document, AcAp::kNone, false);
  }

  if (status == Acad::eOk) {
    AcDb::AcDbDwgVersion version;
    AcDb::MaintenanceReleaseVersion maintenance;
    status = AcApDocument::getDwgVersionFromSaveFormat(
        document->formatForSave(), version, maintenance);

    if (status == Acad::eOk) {
      status =
          document->database()->saveAs(document->fileName(), true, version);
    }
  }

  if (changedCurrent && active) {
    const Acad::ErrorStatus restoreStatus =
        acDocManager->setCurDocument(active, AcAp::kNone, false);

    if (status == Acad::eOk) {
      status = restoreStatus;
    }
  }

  const Acad::ErrorStatus unlockStatus = acDocManager->unlockDocument(document);

  if (status == Acad::eOk) {
    status = unlockStatus;
  }

  return status == Acad::eOk
             ? result(acadctl::NativeActionResultKind::Success)
             : nativeFailure(acadctl::NativeActionResultKind::SaveFailed,
                             status);
}

acadctl::NativeActionResult ObjectArxBridge::close(AcApDocument *document,
                                                   bool discard) {
  AcDbDatabase *database = document->database();
  const int dbmod = acdbGetDbmod(database);

  if (dbmod != 0 && !discard) {
    return result(acadctl::NativeActionResultKind::Dirty);
  }

  if (discard) {
    acdbSetDbmod(database, 0);
  }

  const Acad::ErrorStatus status =
      acDocManager->appContextCloseDocument(document);

  if (status != Acad::eOk && discard) {
    acdbSetDbmod(database, dbmod);
  }

  return status == Acad::eOk
             ? result(acadctl::NativeActionResultKind::Success)
             : nativeFailure(acadctl::NativeActionResultKind::CloseFailed,
                             status);
}

bool ObjectArxBridge::queueDocumentContextDispatch(
    const acadctl::NativeAction &action,
    acadctl::NativeActionResult &failure) {
  if (documentContextDispatch_) {
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, RTERROR);

    return false;
  }

  DocumentContextDispatch::Kind kind;

  switch (action.kind) {
  case acadctl::NativeActionKind::Undo:
    kind = DocumentContextDispatch::Kind::Undo;
    break;
  case acadctl::NativeActionKind::Redo:
    kind = DocumentContextDispatch::Kind::Redo;
    break;
  case acadctl::NativeActionKind::QueueExecutionDriver:
    kind = DocumentContextDispatch::Kind::ExecutionDriver;
    break;
  default:
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, RTERROR);

    return false;
  }

  AcApDocument *target = document(action.document_token);

  if (!target) {
    failure = result(acadctl::NativeActionResultKind::DocumentGone);

    return false;
  }

  if (!matchesDatabase(target, action.database_token)) {
    failure =
        result(acadctl::NativeActionResultKind::DocumentGenerationChanged);

    return false;
  }

  if (kind == DocumentContextDispatch::Kind::ExecutionDriver) {
    if (!lispFunctionsDefined(target)) {
      failure = bridgeFailure(
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR);

      return false;
    }

    if (!target->database()->undoRecording()) {
      failure = result(acadctl::NativeActionResultKind::UndoDisabled);

      return false;
    }
  }

  AcApDocument *previousActive = acDocManager->mdiActiveDocument();

  if (!previousActive) {
    failure = nativeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
                            Acad::eNoDocument);

    return false;
  }

  if (acDocManager->curDocument() != previousActive ||
      !previousActive->isQuiescent()) {
    failure = result(acadctl::NativeActionResultKind::NotQuiescent);

    return false;
  }

  if (!target->isQuiescent()) {
    failure = result(acadctl::NativeActionResultKind::NotQuiescent);

    return false;
  }

  const bool restorePreviousActive = previousActive != target;
  const int pendingInput = acDocManager->inputPending(target);

  if (pendingInput > 0) {
    failure = result(acadctl::NativeActionResultKind::NotQuiescent);

    return false;
  }

  if (pendingInput < 0) {
    failure = bridgeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
                            RTERROR);

    return false;
  }

  documentContextDispatch_.emplace(DocumentContextDispatch{
      action.job_id,
      action.document_token,
      action.database_token,
      static_cast<std::size_t>(
          reinterpret_cast<std::uintptr_t>(previousActive)),
      kind,
      restorePreviousActive,
      result(acadctl::NativeActionResultKind::Success),
      DocumentContextDispatch::Phase::Queued,
  });

  nativeActionCallbacksOutstanding.fetch_add(1, std::memory_order_seq_cst);
  const ACHAR *invocation =
      kind == DocumentContextDispatch::Kind::ExecutionDriver
          ? bridgeProtocol().executionDriverInvocation.kACharPtr()
          : kHistoryCommandInvocation;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->sendStringToExecute(target, invocation, true, false,
                                        false);

  if (scheduleStatus == Acad::eOk) {
    return true;
  }

  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  documentContextDispatch_.reset();
  failure = nativeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
                          scheduleStatus);

  Acad::ErrorStatus restoreStatus = Acad::eOk;

  if (restorePreviousActive &&
      acDocManager->mdiActiveDocument() != previousActive) {
    restoreStatus = acDocManager->activateDocument(previousActive, false);
  }

  if (restoreStatus != Acad::eOk ||
      acDocManager->mdiActiveDocument() != previousActive ||
      acDocManager->curDocument() != previousActive) {
    failure = nativeFailure(
        acadctl::NativeActionResultKind::DocumentContextRestoreFailed,
        restoreStatus == Acad::eOk ? Acad::eInvalidContext : restoreStatus);
  }

  return false;
}

void ObjectArxBridge::scheduleDocumentContextFinalizer() {
  if (!documentContextDispatch_) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;
  dispatch.phase = DocumentContextDispatch::Phase::Finalizing;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->beginExecuteInApplicationContext(finalizeDocumentContextDispatch,
                                                     nullptr);

  if (scheduleStatus == Acad::eOk) {
    return;
  }

  dispatch.dispatchResult = nativeFailure(
      acadctl::NativeActionResultKind::DocumentContextRestoreFailed, scheduleStatus);
  const std::uint64_t jobId = dispatch.jobId;
  acadctl::NativeActionResult dispatchResult = std::move(dispatch.dispatchResult);
  const bool executionDriver =
      dispatch.kind == DocumentContextDispatch::Kind::ExecutionDriver;
  const acadctl::NativeExecutionFinalizationObservation observation =
      finalizationObservation(dispatch);
  documentContextDispatch_.reset();
  if (executionDriver) {
    acadctl::complete_execution_native_action(
        jobId, std::move(dispatchResult), observation);
  } else {
    acadctl::complete_native_action(jobId, std::move(dispatchResult));
  }
  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
}

void ObjectArxBridge::queuedHistoryCommandTerminated(
    const ACHAR *commandName) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->phase != DocumentContextDispatch::Phase::Queued ||
      !commandName) {
    return;
  }

  if (documentContextDispatch_->kind ==
      DocumentContextDispatch::Kind::ExecutionDriver) {
    return;
  }

  const std::size_t commandLength =
      std::char_traits<ACHAR>::length(commandName);
  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(kHistoryCommandName);

  if (commandLength != expectedLength ||
      std::char_traits<ACHAR>::compare(commandName,
                                      kHistoryCommandName,
                                      expectedLength) != 0) {
    return;
  }

  documentContextDispatch_->dispatchResult = bridgeFailure(
      acadctl::NativeActionResultKind::HistoryFailed, RTERROR);
  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::queuedExecutionDriverStarted(const ACHAR *firstLine) {
  if (!documentContextDispatch_ ||
      (documentContextDispatch_->phase !=
           DocumentContextDispatch::Phase::Queued &&
       documentContextDispatch_->phase !=
           DocumentContextDispatch::Phase::Running) ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver ||
      !firstLine) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;

  if (dispatch.driverStarted) {
    if (dispatch.lispDepth == std::numeric_limits<std::uint32_t>::max()) {
      failExecutionDriver();

      return;
    }

    ++dispatch.lispDepth;

    return;
  }

  const std::size_t actualLength =
      std::char_traits<ACHAR>::length(firstLine);
  const ACHAR *driverExpression =
      bridgeProtocol().executionDriverExpression.kACharPtr();
  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(driverExpression);

  if (actualLength != expectedLength ||
      std::char_traits<ACHAR>::compare(
          firstLine, driverExpression, expectedLength) != 0) {
    return;
  }

  dispatch.driverStarted = true;
  dispatch.lispDepth = 1;
}

void ObjectArxBridge::failExecutionDriver() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver ||
      documentContextDispatch_->phase ==
          DocumentContextDispatch::Phase::Finalizing) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;
  finishValueWriter(dispatch.valueWriter, false);

  dispatch.dispatchResult = bridgeFailure(
      acadctl::NativeActionResultKind::ExecutionBridgeFailed,
      RTERROR);
  scheduleExecutionDispatchFinalizer();
}

void ObjectArxBridge::recoverCancelledExecutionDriver() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver ||
      documentContextDispatch_->phase ==
          DocumentContextDispatch::Phase::Finalizing) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;
  finishValueWriter(dispatch.valueWriter, true);

  AcApDocument *target = document(dispatch.documentToken);

  if (!target ||
      !matchesExecutionContext(target, dispatch.databaseToken, target)) {
    dispatch.bridgeSymbolsMayBeRetained =
        dispatch.bridgeSymbolsMayBeRetained ||
        dispatch.stagedFormKind != DocumentContextDispatch::StagedFormKind::None;
    dispatch.dispatchResult = abandonLostExecutionContext(
        dispatch.jobId, target, dispatch.databaseToken, target,
        dispatch.bridgeSymbolsMayBeRetained);
    scheduleExecutionDispatchFinalizer();

    return;
  }

  bool interruptedStepRecorded = false;

  if (dispatch.stagedFormKind ==
      DocumentContextDispatch::StagedFormKind::Evaluator) {
    acadctl::NativeBridgeStepResult interrupted =
        finishEvaluation(stepNativeFailure(RTERROR), false);
    dispatch.bridgeSymbolsMayBeRetained =
        interrupted.bridge_symbols_may_be_retained;
    dispatch.stagedFormKind = DocumentContextDispatch::StagedFormKind::None;
    interruptedStepRecorded =
        acadctl::complete_execution_step(dispatch.jobId,
                                         std::move(interrupted.result));
  } else if (dispatch.stagedFormKind ==
             DocumentContextDispatch::StagedFormKind::ValueVisitor) {
    acadctl::NativeBridgeStepResult interrupted =
        finishEvaluation(stepNativeFailure(RTERROR), false);
    dispatch.bridgeSymbolsMayBeRetained =
        interrupted.bridge_symbols_may_be_retained;
    dispatch.stagedFormKind = DocumentContextDispatch::StagedFormKind::None;
    interruptedStepRecorded = acadctl::complete_execution_step(
        dispatch.jobId, std::move(interrupted.result));
  }

  if (!interruptedStepRecorded) {
    interruptedStepRecorded = acadctl::abandon_execution(
        dispatch.jobId, stepNativeFailure(RTERROR));
  }

  if (!interruptedStepRecorded) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR);
    scheduleExecutionDispatchFinalizer();

    return;
  }

  dispatch.phase = DocumentContextDispatch::Phase::Queued;
  dispatch.driverExitReady = false;
  dispatch.driverStarted = false;
  dispatch.driverEnded = false;
  dispatch.advanceCallbackActive = false;
  dispatch.lispDepth = 0;
  const Acad::ErrorStatus scheduleStatus = acDocManager->sendStringToExecute(
      target, bridgeProtocol().executionDriverInvocation.kACharPtr(), true,
      false, false);

  if (scheduleStatus != Acad::eOk) {
    dispatch.dispatchResult = nativeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed,
        scheduleStatus);
    scheduleExecutionDispatchFinalizer();
  }
}

void ObjectArxBridge::scheduleExecutionDispatchFinalizer() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver) {
    return;
  }

  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::queuedExecutionDriverTerminated(bool cancelled) {
  if (!documentContextDispatch_ ||
      (documentContextDispatch_->phase !=
           DocumentContextDispatch::Phase::Queued &&
       documentContextDispatch_->phase !=
           DocumentContextDispatch::Phase::Running) ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver ||
      !documentContextDispatch_->driverStarted) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;

  if (cancelled) {
    recoverCancelledExecutionDriver();

    return;
  }

  if (dispatch.lispDepth > 1) {
    --dispatch.lispDepth;

    return;
  }

  dispatch.lispDepth = 0;
  dispatch.driverEnded = true;

  if (dispatch.driverExitReady) {
    scheduleExecutionDispatchFinalizer();
  } else if (!dispatch.advanceCallbackActive) {
    failExecutionDriver();
  }
}

void ObjectArxBridge::finishAdvanceCallback(
    bool evaluateStagedForm) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocumentContextDispatch::Kind::ExecutionDriver ||
      documentContextDispatch_->phase ==
          DocumentContextDispatch::Phase::Finalizing) {
    return;
  }

  DocumentContextDispatch &dispatch = *documentContextDispatch_;
  dispatch.advanceCallbackActive = false;

  if (!dispatch.driverEnded) {
    return;
  }

  if (!evaluateStagedForm && dispatch.driverExitReady) {
    scheduleExecutionDispatchFinalizer();

    return;
  }

  failExecutionDriver();
}

int acadctlAdvanceExecution() noexcept {
  ObjectArxBridge *bridge = ObjectArxBridge::commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      (bridge->documentContextDispatch_->phase !=
           ObjectArxBridge::DocumentContextDispatch::Phase::Queued &&
       bridge->documentContextDispatch_->phase !=
           ObjectArxBridge::DocumentContextDispatch::Phase::Running) ||
      bridge->documentContextDispatch_->kind !=
          ObjectArxBridge::DocumentContextDispatch::Kind::ExecutionDriver) {
    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }

  ObjectArxBridge::DocumentContextDispatch &dispatch =
      *bridge->documentContextDispatch_;

  if (!dispatch.driverStarted) {
    dispatch.driverStarted = true;
    dispatch.lispDepth = 1;
  }

  dispatch.phase = ObjectArxBridge::DocumentContextDispatch::Phase::Running;
  dispatch.advanceCallbackActive = true;
  bool evaluateStagedForm = false;
  try {
    AcApDocument *target = bridge->document(dispatch.documentToken);

    if (!target) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::DocumentGone);
    } else if (!matchesDatabase(target, dispatch.databaseToken)) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
    } else if (acDocManager->mdiActiveDocument() != target ||
               acDocManager->curDocument() != target) {
      dispatch.dispatchResult = nativeFailure(
          acadctl::NativeActionResultKind::DocumentContextFailed,
          Acad::eInvalidContext);
    } else if (!bridge->lispFunctionsDefined(target)) {
      dispatch.dispatchResult = bridgeFailure(
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR);
    } else if (!target->database()->undoRecording()) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::UndoDisabled);
    } else {
      const rust::Str evaluator = acadctl::form_evaluator_source();
      const AcString evaluatorText(
          evaluator.data(), AcString::Utf8,
          static_cast<Adesk::UInt32>(evaluator.size()));
      const rust::Str visitor = acadctl::eval_value_visitor_source();
      const AcString visitorText(
          visitor.data(), AcString::Utf8,
          static_cast<Adesk::UInt32>(visitor.size()));

      if (dispatch.stagedFormKind ==
          ObjectArxBridge::DocumentContextDispatch::StagedFormKind::Evaluator) {
        finishValueWriter(dispatch.valueWriter, true);
        acadctl::NativeBridgeStepResult evaluation =
            finishEvaluation(collectEvaluation(dispatch.retainValue));
        dispatch.bridgeSymbolsMayBeRetained =
            evaluation.bridge_symbols_may_be_retained;
        dispatch.stagedFormKind =
            ObjectArxBridge::DocumentContextDispatch::StagedFormKind::None;

        int observationStatus = RTERROR;
        dispatch.undoGroup = observeUndoGroup(observationStatus);

        if (evaluation.result.kind ==
                acadctl::NativeExecutionStepResultKind::Success &&
            dispatch.undoGroup != UndoGroupState::Active) {
          evaluation.result = stepNativeFailure(
              dispatch.undoGroup == UndoGroupState::Unknown
                  ? observationStatus
                  : RTERROR);
        }

        if (!acadctl::complete_execution_step(
                dispatch.jobId, std::move(evaluation.result))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR);
        }
      } else if (dispatch.stagedFormKind ==
                 ObjectArxBridge::DocumentContextDispatch::StagedFormKind::ValueVisitor) {
        activeValueWriter = nullptr;
        acadctl::NativeBridgeStepResult emission =
            finishEvaluation(valueVisitorOutcome(RTNORM));
        dispatch.bridgeSymbolsMayBeRetained =
            emission.bridge_symbols_may_be_retained;
        dispatch.stagedFormKind =
            ObjectArxBridge::DocumentContextDispatch::StagedFormKind::None;
        finishValueWriter(dispatch.valueWriter, false);

        if (!acadctl::complete_execution_step(
                dispatch.jobId, std::move(emission.result))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR);
        }
      }

      while (dispatch.dispatchResult.kind ==
             acadctl::NativeActionResultKind::Success) {
        if (!matchesExecutionContext(target, dispatch.databaseToken,
                                     target)) {
          dispatch.dispatchResult = abandonLostExecutionContext(
              dispatch.jobId, target, dispatch.databaseToken, target,
              dispatch.bridgeSymbolsMayBeRetained);
          break;
        }

        rust::Box<acadctl::NativeExecutionStep> step =
            acadctl::take_execution_step(dispatch.jobId);
        const acadctl::NativeExecutionStepKind kind =
            acadctl::execution_step_kind(*step);

        if (kind == acadctl::NativeExecutionStepKind::Done) {
          if (dispatch.undoGroup != UndoGroupState::Inactive) {
            UndoCommandResult cleanup =
                dispatch.formHandedOff
                    ? rollbackUndoGroup(
                          dispatch.undoGroup,
                          dispatch.executionUndoGroupMayHaveStarted)
                    : runUndoCommand(ACRX_T("_End"),
                                     UndoGroupState::Inactive);
            dispatch.undoGroup = cleanup.state;

            if (cleanup.result.kind !=
                    acadctl::NativeExecutionStepResultKind::Success ||
                dispatch.undoGroup != UndoGroupState::Inactive) {
              dispatch.terminalCleanupFailed = true;
              dispatch.dispatchResult = bridgeFailure(
                  acadctl::NativeActionResultKind::ExecutionBridgeFailed,
                  cleanup.result.native_status == 0
                      ? RTERROR
                      : cleanup.result.native_status);
            }
          }

          if (dispatch.bridgeSymbolsMayBeRetained) {
            const int cleanupStatus = clearExecutionBridgeSymbols();
            dispatch.bridgeSymbolsMayBeRetained = cleanupStatus != RTNORM;

            if (dispatch.bridgeSymbolsMayBeRetained) {
              dispatch.terminalCleanupFailed = true;
              dispatch.dispatchResult = bridgeFailure(
                  acadctl::NativeActionResultKind::ExecutionBridgeSymbolsClearFailed,
                  cleanupStatus);
            }
          }

          break;
        }

        if (kind == acadctl::NativeExecutionStepKind::Invalid) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR);
          break;
        }

        acadctl::NativeExecutionStepResult stepResult = stepSuccess();
        UndoCommandResult undoTransition{stepSuccess(), dispatch.undoGroup};

        switch (kind) {
        case acadctl::NativeExecutionStepKind::BeginUndoGroup:
          undoTransition =
              runUndoCommand(ACRX_T("_Begin"), UndoGroupState::Active);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          dispatch.executionUndoGroupMayHaveStarted =
              dispatch.undoGroup != UndoGroupState::Inactive;
          break;
        case acadctl::NativeExecutionStepKind::EvaluateForm: {
          dispatch.formHandedOff = true;
          const bool retainValue =
              acadctl::execution_step_retain_value(*step);
          acadctl::NativeBridgeStepResult staging = stageEvaluation(
              acadctl::execution_step_source(*step), evaluatorText,
              retainValue);
          dispatch.bridgeSymbolsMayBeRetained =
              staging.bridge_symbols_may_be_retained;

          if (staging.result.kind ==
              acadctl::NativeExecutionStepResultKind::Success) {
            dispatch.stagedFormKind =
                ObjectArxBridge::DocumentContextDispatch::StagedFormKind::Evaluator;
            dispatch.retainValue = retainValue;
            evaluateStagedForm = true;
            break;
          }

          stepResult = std::move(staging.result);
          break;
        }

        case acadctl::NativeExecutionStepKind::CommitUndoGroup:
        case acadctl::NativeExecutionStepKind::CloseEmptyUndoGroup:
          undoTransition =
              runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecutionStepKind::RollbackUndoGroup:
          undoTransition = rollbackUndoGroup(
              dispatch.undoGroup, dispatch.executionUndoGroupMayHaveStarted);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecutionStepKind::ClearRetainedEvalValue: {
          const int cleanupStatus = clearExecutionBridgeSymbols();
          stepResult = cleanupStatus == RTNORM
                           ? stepSuccess()
                           : stepNativeFailure(cleanupStatus);
          dispatch.bridgeSymbolsMayBeRetained = cleanupStatus != RTNORM;
          break;
        }

        case acadctl::NativeExecutionStepKind::EmitEvalValue: {
          rust::Box<acadctl::NativeValueWriter> writer =
              acadctl::begin_eval_value(
                  dispatch.jobId, dispatch.documentToken,
                  dispatch.databaseToken);

          if (!acadctl::value_writer_active(*writer)) {
            acadctl::finish_value_writer(std::move(writer));
            acadctl::NativeBridgeStepResult emission =
                finishEvaluation(stepSuccess(), false);
            stepResult = std::move(emission.result);
            dispatch.bridgeSymbolsMayBeRetained =
                emission.bridge_symbols_may_be_retained;
            break;
          }

          int preparationStatus = clearExecutionBridgeSymbols(false);

          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(
                bridgeProtocol().stagedFormSymbol.kACharPtr(), visitorText);
          }

          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(
                bridgeProtocol().statusSymbol.kACharPtr(),
                bridgeProtocol().pendingStatus);
          }

          if (preparationStatus != RTNORM || activeValueWriter) {
            acadctl::invalidate_value_writer(*writer);
            acadctl::finish_value_writer(std::move(writer));
            acadctl::NativeBridgeStepResult emission = finishEvaluation(
                stepNativeFailure(preparationStatus == RTNORM
                                      ? RTERROR
                                      : preparationStatus),
                false);
            stepResult = std::move(emission.result);
            dispatch.bridgeSymbolsMayBeRetained =
                emission.bridge_symbols_may_be_retained;
            break;
          }

          dispatch.valueWriter.emplace(std::move(writer));
          activeValueWriter = &**dispatch.valueWriter;
          dispatch.stagedFormKind =
              ObjectArxBridge::DocumentContextDispatch::StagedFormKind::ValueVisitor;
          dispatch.bridgeSymbolsMayBeRetained = true;
          evaluateStagedForm = true;
          break;
        }

        case acadctl::NativeExecutionStepKind::Invalid:
        case acadctl::NativeExecutionStepKind::Done:
          break;
        }

        if (evaluateStagedForm) {
          break;
        }

        if (!acadctl::complete_execution_step(dispatch.jobId,
                                               std::move(stepResult))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR);
        }
      }
    }
  } catch (...) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR);
  }

  const int returnStatus = evaluateStagedForm ? acedRetT() : acedRetNil();

  if (returnStatus != RTNORM) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, returnStatus);
    evaluateStagedForm = false;
  }

  if (!evaluateStagedForm) {
    finishValueWriter(dispatch.valueWriter, false);
  }

  if (!evaluateStagedForm && returnStatus == RTNORM) {
    dispatch.driverExitReady = true;
  }

  bridge->finishAdvanceCallback(evaluateStagedForm);

  return returnStatus == RTNORM ? RSRSLT : RSERR;
}

void ObjectArxBridge::runQueuedHistoryCommand() {
  ObjectArxBridge *bridge = commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      bridge->documentContextDispatch_->phase !=
          DocumentContextDispatch::Phase::Queued) {
    return;
  }

  DocumentContextDispatch &dispatch = *bridge->documentContextDispatch_;

  if (dispatch.kind == DocumentContextDispatch::Kind::ExecutionDriver) {
    return;
  }

  dispatch.phase = DocumentContextDispatch::Phase::Running;
  AcApDocument *target = bridge->document(dispatch.documentToken);

  if (!target) {
    dispatch.dispatchResult =
        result(acadctl::NativeActionResultKind::DocumentGone);
  } else if (!matchesDatabase(target, dispatch.databaseToken)) {
    dispatch.dispatchResult =
        result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
  } else if (acDocManager->mdiActiveDocument() != target ||
             acDocManager->curDocument() != target) {
    dispatch.dispatchResult = nativeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed,
        Acad::eInvalidContext);
  } else {
    int undoStatus = RTERROR;
    const UndoGroupState undoState = observeUndoGroup(undoStatus);

    if (undoState == UndoGroupState::Active) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::NotQuiescent);
    } else if (undoState == UndoGroupState::Unknown) {
      dispatch.dispatchResult = bridgeFailure(
          acadctl::NativeActionResultKind::HistoryFailed, undoStatus);
    } else {
      const int status = acedCommandS(
          RTSTR,
          dispatch.kind == DocumentContextDispatch::Kind::Redo
              ? ACRX_T("_.REDO")
              : ACRX_T("_.U"),
          RTNONE);

      if (acDocManager->mdiActiveDocument() != target ||
          acDocManager->curDocument() != target) {
        dispatch.dispatchResult = nativeFailure(
            acadctl::NativeActionResultKind::DocumentContextFailed,
            Acad::eInvalidContext);
      } else if (!matchesDatabase(target, dispatch.databaseToken)) {
        dispatch.dispatchResult =
            result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
      } else if (status != RTNORM) {
        dispatch.dispatchResult = bridgeFailure(
            acadctl::NativeActionResultKind::HistoryFailed, status);
      }
    }
  }

  bridge->scheduleDocumentContextFinalizer();
}

acadctl::NativeExecutionFinalizationObservation
ObjectArxBridge::finalizationObservation(
    const DocumentContextDispatch &dispatch) {
  return {
      dispatch.undoGroup != UndoGroupState::Inactive,
      dispatch.bridgeSymbolsMayBeRetained,
      dispatch.stagedFormKind != DocumentContextDispatch::StagedFormKind::None,
      dispatch.valueWriter.has_value(),
      dispatch.terminalCleanupFailed,
  };
}

void ObjectArxBridge::finalizeDocumentContextDispatch(void *) {
  NativeActionCallbackLease callbackLease;
  ObjectArxBridge *bridge = commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      bridge->documentContextDispatch_->phase !=
          DocumentContextDispatch::Phase::Finalizing) {
    return;
  }

  DocumentContextDispatch &dispatch = *bridge->documentContextDispatch_;

  if (dispatch.restorePreviousActive) {
    AcApDocument *previousActive =
        bridge->document(dispatch.previousActiveToken);
    Acad::ErrorStatus restoreStatus = Acad::eNoDocument;

    if (previousActive) {
      restoreStatus = acDocManager->activateDocument(previousActive, false);
    }

    if (restoreStatus != Acad::eOk ||
        acDocManager->mdiActiveDocument() != previousActive ||
        acDocManager->curDocument() != previousActive) {
      dispatch.dispatchResult = nativeFailure(
          acadctl::NativeActionResultKind::DocumentContextRestoreFailed,
          restoreStatus == Acad::eOk ? Acad::eInvalidContext : restoreStatus);
    }
  }

  bridge->refreshDocumentSnapshot();
  const std::uint64_t jobId = dispatch.jobId;
  acadctl::NativeActionResult dispatchResult = std::move(dispatch.dispatchResult);
  const bool executionDriver =
      dispatch.kind == DocumentContextDispatch::Kind::ExecutionDriver;
  const acadctl::NativeExecutionFinalizationObservation observation =
      finalizationObservation(dispatch);
  bridge->documentContextDispatch_.reset();
  if (executionDriver) {
    acadctl::complete_execution_native_action(
        jobId, std::move(dispatchResult), observation);
  } else {
    acadctl::complete_native_action(jobId, std::move(dispatchResult));
  }
  scheduleNextNativeAction();
}

void ObjectArxBridge::publishDocumentSnapshot() {
  rust::Vec<acadctl::NativeDocumentSnapshot> states;

  for (DocumentSubscription &subscription : subscriptions_) {
    refreshSubscription(subscription);

    if (!subscription.database) {
      continue;
    }

    AcApDocument *document = subscription.document;
    const bool named = document->isNamedDrawing();
    const AcString name(named ? document->fileName() : document->docTitle());
    states.push_back(acadctl::NativeDocumentSnapshot{
        static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(document)),
        static_cast<std::size_t>(
            reinterpret_cast<std::uintptr_t>(subscription.database)),
        rust::String(name.utf8Ptr()),
        named,
        acdbGetDbmod(subscription.database) != 0,
        document->isReadOnly(),
    });
  }

  acadctl::publish_document_snapshot(std::move(states));
}

void ObjectArxBridge::refreshDocumentSnapshot() {
  drainDatabaseChanges();
  documentSnapshotStale_.store(false, std::memory_order_relaxed);
  publishDocumentSnapshot();
}

void ObjectArxBridge::refreshDocumentSnapshotIfStale() {
  drainDatabaseChanges();

  if (!documentSnapshotStale_.exchange(false, std::memory_order_relaxed)) {
    return;
  }

  publishDocumentSnapshot();
}

void ObjectArxBridge::drainDatabaseChanges() {
  for (DocumentSubscription &subscription : subscriptions_) {
    drainDatabaseChanges(subscription);
  }
}

void ObjectArxBridge::drainDatabaseChanges(
    DocumentSubscription &subscription) {
  if (subscription.databaseReactor &&
      subscription.databaseReactor->takeChanged()) {
    documentSnapshotStale_.store(true, std::memory_order_relaxed);
  }
}

void ObjectArxBridge::eraseDatabaseReactor(DatabaseReactor *reactor) {
  const auto owned = std::find_if(
      databaseReactors_.begin(), databaseReactors_.end(),
      [reactor](const auto &candidate) { return candidate.get() == reactor; });

  if (owned != databaseReactors_.end()) {
    databaseReactors_.erase(owned);
  }
}

void ObjectArxBridge::detachDatabaseReactor(
    DocumentSubscription &subscription) {
  if (!subscription.databaseReactor) {
    return;
  }

  if (subscription.databaseReactor->databaseGone()) {
    drainDatabaseChanges(subscription);
    DatabaseReactor *reactor = subscription.databaseReactor;
    subscription.databaseReactor = nullptr;
    eraseDatabaseReactor(reactor);

    return;
  }

  const Acad::ErrorStatus status =
      subscription.database
          ? subscription.database->removeReactor(
                subscription.databaseReactor)
          : Acad::eNullPtr;
  drainDatabaseChanges(subscription);

  if (status == Acad::eOk || status == Acad::eKeyNotFound) {
    DatabaseReactor *reactor = subscription.databaseReactor;
    subscription.databaseReactor = nullptr;
    eraseDatabaseReactor(reactor);

    return;
  }

  syslog(LOG_ERR, "acadctl could not detach a database observer: %d",
         static_cast<int>(status));
  databaseReactorOwnershipUncertain_ = true;
  subscription.databaseReactor = nullptr;
}

void ObjectArxBridge::refreshSubscription(DocumentSubscription &subscription) {
  if (subscription.databaseReactor &&
      subscription.databaseReactor->databaseGone()) {
    AcDbDatabase *retiredDatabase = subscription.database;
    detachDatabaseReactor(subscription);
    subscription.database = nullptr;
    subscription.retiredDatabase = retiredDatabase;
    subscription.lispFunctionsDefined = false;
  }

  AcDbDatabase *database = subscription.document->database();

  if (database && database == subscription.retiredDatabase) {
    documentSnapshotStale_.store(true, std::memory_order_relaxed);

    return;
  }

  subscription.retiredDatabase = nullptr;

  if (subscription.database != database) {
    detachDatabaseReactor(subscription);
    subscription.database = database;
    subscription.lispFunctionsDefined = false;
  }

  if (subscription.databaseReactor) {
    return;
  }

  if (!subscription.database || databaseReactorOwnershipUncertain_) {
    documentSnapshotStale_.store(true, std::memory_order_relaxed);

    return;
  }

  auto ownedReactor = std::make_unique<DatabaseReactor>();
  DatabaseReactor *reactor = ownedReactor.get();
  databaseReactors_.push_back(std::move(ownedReactor));
  const Acad::ErrorStatus status =
      subscription.database->addReactor(reactor);

  if (status == Acad::eOk) {
    subscription.databaseReactor = reactor;

    return;
  }

  documentSnapshotStale_.store(true, std::memory_order_relaxed);

  if (status == Acad::eDuplicateKey) {
    databaseReactorOwnershipUncertain_ = true;
  } else {
    eraseDatabaseReactor(reactor);
  }

  syslog(LOG_ERR, "acadctl could not attach a database observer: %d",
         static_cast<int>(status));
}

void ObjectArxBridge::subscribe(AcApDocument *document) {
  const auto alreadySubscribed =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocumentSubscription &subscription) {
                     return subscription.document == document;
                   });

  if (alreadySubscribed != subscriptions_.end()) {
    return;
  }

  subscriptions_.push_back(
      DocumentSubscription{document, nullptr, nullptr, false, nullptr});
  refreshSubscription(subscriptions_.back());
}

void ObjectArxBridge::databaseWillBeDestroyed(AcDbDatabase *database) {
  bool retiredSubscription = false;

  for (DocumentSubscription &subscription : subscriptions_) {
    if (subscription.database != database) {
      continue;
    }

    detachDatabaseReactor(subscription);
    subscription.database = nullptr;
    subscription.retiredDatabase = database;
    subscription.lispFunctionsDefined = false;
    retiredSubscription = true;
  }

  if (retiredSubscription) {
    documentSnapshotStale_.store(true, std::memory_order_relaxed);
  }
}

void ObjectArxBridge::actionTargetWillBeDestroyed(AcApDocument *document) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->phase != DocumentContextDispatch::Phase::Queued) {
    return;
  }

  const std::size_t documentToken = static_cast<std::size_t>(
      reinterpret_cast<std::uintptr_t>(document));

  if (documentContextDispatch_->documentToken != documentToken) {
    return;
  }

  documentContextDispatch_->dispatchResult =
      result(acadctl::NativeActionResultKind::DocumentGone);
  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::unsubscribe(AcApDocument *document) {
  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocumentSubscription &candidate) {
                     return candidate.document == document;
                   });

  if (subscription == subscriptions_.end()) {
    return;
  }

  detachDatabaseReactor(*subscription);
  subscriptions_.erase(subscription);
}

std::unique_ptr<ObjectArxBridge> objectArxBridge;

void processNextAction(void *) {
  NativeActionCallbackLease callbackLease;

  if (objectArxBridge) {
    objectArxBridge->processNextAction();
  }
}

} // namespace

extern "C" int acadctl_wake_native_actions() {
  nativeActionCallbacksOutstanding.fetch_add(1, std::memory_order_seq_cst);

  if (!acceptNativeActionWakes.load(std::memory_order_seq_cst)) {
    nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);

    return static_cast<int>(Acad::eInvalidContext);
  }

  const int status =
      static_cast<int>(acDocManager->beginExecuteInApplicationContext(
          processNextAction, nullptr));

  if (status != 0) {
    nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  }

  return status;
}

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                           void *applicationId) {
  switch (message) {
  case AcRx::kInitAppMsg: {
    acrxDynamicLinker->registerAppMDIAware(applicationId);
    try {
      objectArxBridge = std::make_unique<ObjectArxBridge>();
    } catch (...) {
      syslog(LOG_ERR, "acadctl plugin could not allocate its native bridge");

      return AcRx::kRetError;
    }

    const auto failInitialization = []() {
      acceptNativeActionWakes.store(false, std::memory_order_seq_cst);
      acadctl::stop_rpc_server();

      if (nativeActionCallbacksOutstanding.load(std::memory_order_seq_cst) !=
              0 ||
          !objectArxBridge->stop()) {
        syslog(LOG_ERR,
               "acadctl plugin initialization failed after AutoCAD retained "
               "a native callback; the inert module will remain loaded");

        return AcRx::kRetOK;
      }

      objectArxBridge.reset();

      return AcRx::kRetError;
    };

    try {
      const Acad::ErrorStatus startStatus = objectArxBridge->start();

      if (startStatus != Acad::eOk) {
        syslog(LOG_ERR,
               "acadctl plugin could not register its native command: %d",
               static_cast<int>(startStatus));

        return failInitialization();
      }
    } catch (...) {
      syslog(LOG_ERR, "acadctl plugin initialization failed");

      return failInitialization();
    }

    rust::String error = acadctl::start_rpc_server();

    if (!error.empty()) {
      syslog(LOG_ERR, "acadctl plugin failed to start: %s", error.c_str());

      return failInitialization();
    }

    break;
  }

  case AcRx::kLoadDwgMsg: {
    const int status = defineLispFunctions();

    if (objectArxBridge) {
      objectArxBridge->setLispFunctionsDefined(acDocManager->curDocument(),
                                               status == RTNORM);
    }

    if (status != RTNORM) {
      syslog(LOG_ERR,
             "acadctl plugin could not define its AutoLISP functions: %d",
             status);
    }

    break;
  }

  case AcRx::kUnloadDwgMsg: {
    AcApDocument *document = acDocManager->curDocument();
    const int status = undefineLispFunctions();

    if (objectArxBridge) {
      objectArxBridge->setLispFunctionsDefined(document, false);
    }

    if (status != RTNORM) {
      syslog(LOG_ERR,
             "acadctl plugin could not undefine its AutoLISP functions: %d",
             status);
    }

    break;
  }

  case AcRx::kUnloadAppMsg:
    acceptNativeActionWakes.store(false, std::memory_order_seq_cst);
    acadctl::stop_rpc_server();

    if (nativeActionCallbacksOutstanding.load(std::memory_order_seq_cst) !=
        0) {
      syslog(LOG_ERR,
             "acadctl plugin cannot unload while a native action callback is "
             "outstanding");

      return AcRx::kRetError;
    }

    if (objectArxBridge && !objectArxBridge->stop()) {
      syslog(LOG_ERR,
             "acadctl plugin cannot unload while AutoCAD may retain a "
             "database reactor");

      return AcRx::kRetError;
    }

    objectArxBridge.reset();
    break;
  default:
    break;
  }

  return AcRx::kRetOK;
}
