#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include "accmd.h"
#include "acdocman.h"
#include "aced.h"
#include "acedCmdNF.h"
#include "acedads.h"
#include "acestext.h"
#include "acgs.h"
#include "acutads.h"
#include "adscodes.h"
#include "dbhandle.h"
#include "dbmain.h"
#include "gs.h"
#include "rxregsvc.h"
#ifdef ACADCTL_HAS_ATIL
#include "Image.h"
#include "RgbModel.h"
#include "Size.h"
#include "acutmem.h"
#include "dbobjptr.h"
#include "dbvisualstyle.h"
#include "dbxutil.h"
#endif
#include <algorithm>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <filesystem>
#include <limits>
#include <memory>
#include <optional>
#include <syslog.h>
#include <type_traits>
#include <vector>

int acdbGetDbmod(AcDbDatabase* database);
int acdbSetDbmod(AcDbDatabase* database, int value);
extern "C" int acadctl_wake_native_actions();

namespace {

std::atomic<std::uint32_t> nativeActionCallbacksOutstanding{0};
std::atomic<bool> acceptNativeActionWakes{true};
const ACHAR kHistoryCommandGroup[] = ACRX_T("ACADCTL_INTERNAL");
const ACHAR kHistoryCommandName[] = ACRX_T("ACADCTL_INTERNAL_HISTORY");
const ACHAR kHistoryCommandInvocation[] = ACRX_T("ACADCTL_INTERNAL_HISTORY\n");
const ACHAR kExecutionDriverExpression[] = ACRX_T("(actl:_drive-execution)");
const ACHAR kExecutionDriverInvocation[] = ACRX_T("(actl:_drive-execution)\n");
const ACHAR kEvalMarker[] = ACRX_T("actl:_eval");
const ACHAR kEmitRetainedValueExpression[] =
    ACRX_T("(actl:_emit-retained-value)");
const ACHAR kOutputEventFunction[] = ACRX_T("actl:_output-event");
const ACHAR kAdvanceExecutionFunction[] = ACRX_T("actl:_advance-execution");
const ACHAR kSourceSymbol[] = ACRX_T("actl:*bridge-source*");
const ACHAR kStagedFormSymbol[] = ACRX_T("actl:*bridge-staged-form*");
const ACHAR kStatusSymbol[] = ACRX_T("actl:*bridge-status*");
const ACHAR kErrorSymbol[] = ACRX_T("actl:*bridge-error*");
const ACHAR kErrnoSymbol[] = ACRX_T("actl:*bridge-errno*");
const ACHAR kValueSymbol[] = ACRX_T("actl:*bridge-value*");
const ACHAR kPendingStatus[] = ACRX_T("pending");
#ifdef ACADCTL_HAS_ATIL
const ACHAR kRealisticVisualStyle[] = ACRX_T("Realistic");
#endif
constexpr std::size_t kValueChunkCaptureUnits = 4096;
constexpr int kMaximumCaptureDimension = 16384;
constexpr std::size_t kMaximumCaptureBytes = std::size_t{128} * 1024 * 1024;

class NativeActionCallbackLease final {
public:
  ~NativeActionCallbackLease() {
    nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  }
};

class DatabaseReactor final : public AcDbDatabaseReactor {
public:
  void objectAppended(const AcDbDatabase*, const AcDbObject*) override {
    markChanged();
  }

  void objectUnAppended(const AcDbDatabase*, const AcDbObject*) override {
    markChanged();
  }

  void objectReAppended(const AcDbDatabase*, const AcDbObject*) override {
    markChanged();
  }

  void objectOpenedForModify(const AcDbDatabase*, const AcDbObject*) override {
    markChanged();
  }

  void objectModified(const AcDbDatabase*, const AcDbObject*) override {
    markChanged();
  }

  void objectErased(const AcDbDatabase*, const AcDbObject*, bool) override {
    markChanged();
  }

  void headerSysVarWillChange(const AcDbDatabase*, const ACHAR*) override {
    markChanged();
  }

  void headerSysVarChanged(const AcDbDatabase*, const ACHAR*, bool) override {
    markChanged();
  }

  void proxyResurrectionCompleted(const AcDbDatabase*, const ACHAR*,
                                  AcDbObjectIdArray&) override {
    markChanged();
  }

  void goodbye(const AcDbDatabase*) override {
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
  void markChanged() { changed_.store(1, std::memory_order_relaxed); }

  std::atomic<std::uint32_t> changed_{0};
  std::atomic<bool> databaseGone_{false};
};

static_assert(std::atomic<std::uint32_t>::is_always_lock_free);

struct DocSubscription {
  AcApDocument* document;
  AcDbDatabase* database;
  AcDbDatabase* retiredDatabase;
  bool lispFunctionsDefined;
  DatabaseReactor* databaseReactor;
};

acadctl::NativeActionResult result(acadctl::NativeActionResultKind kind);

acadctl::NativeActionResult nativeFailure(acadctl::NativeActionResultKind kind,
                                          Acad::ErrorStatus status);

acadctl::NativeActionResult bridgeFailure(acadctl::NativeActionResultKind kind,
                                          int status);

struct ViewportCaptureResult {
  acadctl::NativeCaptureResult metadata;
  std::vector<std::uint8_t> pixels;
};

ViewportCaptureResult captureResult(acadctl::NativeCaptureResultKind kind,
                                    const char* detail = "");

int acadctlAdvanceExecution() noexcept;
int acadctlOutputEvent() noexcept;
int undefineLispFunctions();

enum class UndoGroupState { Inactive, Active, Unknown };

class ObjectArxBridge {
public:
  ObjectArxBridge();

  ~ObjectArxBridge();

  Acad::ErrorStatus start();

  bool stop();

  void processNextAction();

  void setLispFunctionsDefined(AcApDocument* document, bool defined);

private:
  AcApDocument* document(std::size_t token);

  bool lispFunctionsDefined(AcApDocument* document) const;

  bool applicationContextBlocked(AcApDocument* target = nullptr) const;

  acadctl::NativeActionResult open(rust::Str path);

  acadctl::NativeActionResult switchTo(AcApDocument* document);

  acadctl::NativeActionResult save(AcApDocument* document, rust::Str path);

  acadctl::NativeActionResult close(AcApDocument* document, bool discard);

  ViewportCaptureResult capture(AcApDocument* document);

  bool queueDocumentContextDispatch(const acadctl::NativeAction& action,
                                    acadctl::NativeActionResult& failure);

  void scheduleDocumentContextFinalizer();

  void scheduleExecutionDispatchFinalizer();

  void queuedHistoryCommandTerminated(const ACHAR* commandName);

  void queuedExecutionDriverStarted(const ACHAR* firstLine);

  void queuedExecutionDriverTerminated(bool cancelled);

  void recoverCancelledExecutionDriver();

  void failExecutionDriver();

  enum class AdvanceCompletion { EvaluateStagedForm, ExitReady, ExitFailed };

  void finishAdvanceCallback(AdvanceCompletion completion);

  struct DocContextDispatch {
    enum class Phase { Queued, Running, Finalizing };
    enum class Kind { Undo, Redo, ExecDriver };
    enum class StagedFormKind { None, Evaluator, EvalValueEmitter };
    enum class ExecDriverLifecycle {
      AwaitingStart,
      Running,
      InCallback,
      EndedDuringCallback,
      AwaitingEnd,
      Finalizing,
    };

    std::uint64_t jobId;
    std::size_t documentToken;
    std::size_t databaseToken;
    std::size_t previousActiveToken;
    std::size_t previousActiveDatabaseToken;
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
    bool terminalCleanupFailed = false;
    ExecDriverLifecycle driverLifecycle = ExecDriverLifecycle::AwaitingStart;
    std::uint32_t lispDepth = 0;
    std::optional<rust::Box<acadctl::NativeOutputPort>> outputPort =
        std::nullopt;
  };

  static acadctl::NativeExecFinalizationObservation
  finalizationObservation(const DocContextDispatch& dispatch);

  static void runQueuedHistoryCommand();

  static void finalizeDocumentContextDispatch(void* data);

  friend int acadctlAdvanceExecution() noexcept;
  friend int acadctlOutputEvent() noexcept;

  void publishDocumentSnapshot();

  void refreshDocumentSnapshot();

  void refreshDocumentSnapshotIfStale();

  void drainDatabaseChanges();

  void drainDatabaseChanges(DocSubscription& subscription);

  void eraseDatabaseReactor(DatabaseReactor* reactor);

  void detachDatabaseReactor(DocSubscription& subscription);

  void refreshSubscription(DocSubscription& subscription);

  void databaseWillBeDestroyed(AcDbDatabase* database);

  void actionTargetWillBeDestroyed(AcApDocument* document);

  void subscribe(AcApDocument* document);

  void unsubscribe(AcApDocument* document);

  class DocReactor final : public AcApDocManagerReactor {
  public:
    explicit DocReactor(ObjectArxBridge& bridge) : bridge_(bridge) {}

    void documentCreated(AcApDocument* document) override {
      bridge_.subscribe(document);
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentToBeDestroyed(AcApDocument* document) override {
      bridge_.actionTargetWillBeDestroyed(document);
      bridge_.unsubscribe(document);
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentTitleUpdated(AcApDocument*) override {
      bridge_.refreshDocumentSnapshot();
    }

    void documentBecameCurrent(AcApDocument*) override {
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

    void documentActivated(AcApDocument*) override {
      bridge_.refreshDocumentSnapshot();
      acadctl::native_state_may_be_ready();
    }

  private:
    ObjectArxBridge& bridge_;
  };

  class EditorReactor final : public AcEditorReactor {
  public:
    explicit EditorReactor(ObjectArxBridge& bridge) : bridge_(bridge) {}

    void lispWillStart(const ACHAR* firstLine) override {
      bridge_.queuedExecutionDriverStarted(firstLine);
    }

    void commandEnded(const ACHAR*) override {
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandCancelled(const ACHAR* commandName) override {
      bridge_.queuedHistoryCommandTerminated(commandName);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandFailed(const ACHAR* commandName) override {
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

    void saveComplete(AcDbDatabase*, const ACHAR*) override {
      bridge_.refreshDocumentSnapshot();
    }

    void abortSave(AcDbDatabase*) override {
      bridge_.refreshDocumentSnapshot();
    }

    void curDocOpenUpgraded(AcDbDatabase*, const CAdUiPathname&) override {
      bridge_.refreshDocumentSnapshot();
    }

    void curDocOpenDowngraded(AcDbDatabase*, const CAdUiPathname&) override {
      bridge_.refreshDocumentSnapshot();
    }

    void databaseToBeDestroyed(AcDbDatabase* database) override {
      bridge_.databaseWillBeDestroyed(database);
    }

  private:
    ObjectArxBridge& bridge_;
  };

  std::vector<DocSubscription> subscriptions_;
  std::vector<std::unique_ptr<DatabaseReactor>> databaseReactors_;
  DocReactor documentReactor_;
  EditorReactor editorReactor_;
  std::atomic<bool> documentSnapshotStale_{false};
  bool databaseReactorOwnershipUncertain_ = false;
  bool historyCommandRegistered_ = false;
  std::optional<DocContextDispatch> documentContextDispatch_;

  static ObjectArxBridge* commandBridge_;
};

acadctl::NativeActionResult result(acadctl::NativeActionResultKind kind) {
  return {kind, 0, rust::String()};
}

acadctl::NativeActionResult nativeFailure(acadctl::NativeActionResultKind kind,
                                          Acad::ErrorStatus status) {
  const AcString detail(acadErrorStatusText(status));

  return {kind, static_cast<std::int32_t>(status),
          rust::String(detail.utf8Ptr())};
}

acadctl::NativeActionResult bridgeFailure(acadctl::NativeActionResultKind kind,
                                          int status) {
  return {kind, status, rust::String()};
}

ViewportCaptureResult captureResult(acadctl::NativeCaptureResultKind kind,
                                    const char* detail) {
  return {{kind, 0, 0, 0, acadctl::NativePixelFormat::Invalid,
           acadctl::NativeRowOrder::Invalid, false, rust::String(detail)},
          {}};
}

acadctl::NativeExecStepResult stepSuccess() {
  return {acadctl::NativeExecStepResultKind::Success, 0, 0, rust::String(), 0};
}

acadctl::NativeExecStepResult stepNativeFailure(int status) {
  return {acadctl::NativeExecStepResultKind::NativeError, status, 0,
          rust::String(), 0};
}

struct ResbufDeleter {
  void operator()(resbuf* value) const {
    if (value) {
      acutRelRb(value);
    }
  }
};

using ResbufPtr = std::unique_ptr<resbuf, ResbufDeleter>;

constexpr int kOutputEventFunctionCode = 1;
constexpr int kAdvanceExecutionFunctionCode = 2;

struct BoundedNativeText {
  rust::String text;
  bool truncated;
};

std::size_t boundedWideChunkLength(const ACHAR* text);
BoundedNativeText boundedDiagnostic(const ACHAR* text);
int integerValue(const resbuf* value);
int clearSymbol(const ACHAR* name);
bool matchesExecutionContext(AcApDocument* document, std::size_t databaseToken,
                             AcApDocument* expectedActive);

void finishOutputPort(
    std::optional<rust::Box<acadctl::NativeOutputPort>>& retainedPort,
    bool invalidate) {
  if (!retainedPort) {
    return;
  }

  if (invalidate) {
    acadctl::invalidate_output_port(**retainedPort);
  }

  rust::Box<acadctl::NativeOutputPort> port = std::move(*retainedPort);
  retainedPort.reset();
  acadctl::finish_output_port(std::move(port));
}

std::size_t boundedWideChunkLength(const ACHAR* text) {
  const std::size_t captureUnits = kValueChunkCaptureUnits;
  std::size_t length = 0;

  while (length < captureUnits && text[length] != 0) {
    ++length;
  }

  if constexpr (sizeof(ACHAR) == 2) {
    if (length == captureUnits && text[length] != 0) {
      const auto last = static_cast<std::uint32_t>(
          static_cast<std::make_unsigned_t<ACHAR>>(text[length - 1]));
      const auto next = static_cast<std::uint32_t>(
          static_cast<std::make_unsigned_t<ACHAR>>(text[length]));

      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 &&
          next <= 0xdfff) {
        --length;
      }
    }
  }

  return length;
}

BoundedNativeText boundedDiagnostic(const ACHAR* text) {
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
      const auto last = static_cast<std::uint32_t>(
          static_cast<std::make_unsigned_t<ACHAR>>(text[length - 1]));
      const auto next = static_cast<std::uint32_t>(
          static_cast<std::make_unsigned_t<ACHAR>>(text[length]));

      if (last >= 0xd800 && last <= 0xdbff && next >= 0xdc00 &&
          next <= 0xdfff) {
        --length;
      }
    }
  }

  const AcString captured(text, static_cast<Adesk::UInt32>(length));
  const char* utf8 = captured.utf8Ptr();

  if (!utf8) {
    return {rust::String(), truncated};
  }

  return {rust::String(utf8), truncated};
}

acadctl::NativeLispOutputEvent
lispOutputEvent(int code, acadctl::NativeLispPayloadKind payloadKind) {
  return {code, payloadKind, 0, 0.0, false};
}

bool writeLispOutputEvent(acadctl::NativeOutputPort& port,
                          acadctl::NativeLispOutputEvent event,
                          rust::Str text = rust::Str()) {
  return acadctl::write_lisp_output_event(port, event, text) ==
         acadctl::NativeOutputWriteResult::Continue;
}

bool writeInvalidLispOutputEvent(acadctl::NativeOutputPort& port) {
  return writeLispOutputEvent(
      port, lispOutputEvent(0, acadctl::NativeLispPayloadKind::Invalid));
}

bool writePrivateStringPayload(acadctl::NativeOutputPort& port, int code,
                               const ACHAR* text) {
  if (!text) {
    return writeInvalidLispOutputEvent(port);
  }

  const std::size_t length = boundedWideChunkLength(text);

  if (text[length] != 0) {
    return writeInvalidLispOutputEvent(port);
  }

  const AcString value(text, static_cast<Adesk::UInt32>(length));
  const char* utf8 = value.utf8Ptr();

  if (!utf8) {
    return writeInvalidLispOutputEvent(port);
  }

  acadctl::NativeLispOutputEvent event =
      lispOutputEvent(code, acadctl::NativeLispPayloadKind::String);
  event.has_text = true;

  return writeLispOutputEvent(port, event, rust::Str(utf8, std::strlen(utf8)));
}

bool writePrivateEntityPayload(acadctl::NativeOutputPort& port, int code,
                               const ads_name name) {
  acadctl::NativeLispOutputEvent event =
      lispOutputEvent(code, acadctl::NativeLispPayloadKind::Entity);
  AcDbObjectId objectId;

  if (acdbGetObjectId(objectId, name) != Acad::eOk || objectId.isNull()) {
    return writeLispOutputEvent(port, event);
  }

  ACHAR handleText[AcDbHandle::kStrSiz]{};

  if (!objectId.handle().getIntoAsciiBuffer(handleText)) {
    return writeLispOutputEvent(port, event);
  }

  const AcString handle(handleText);
  const char* utf8 = handle.utf8Ptr();

  if (!utf8) {
    return writeInvalidLispOutputEvent(port);
  }

  event.has_text = true;

  return writeLispOutputEvent(port, event, rust::Str(utf8, std::strlen(utf8)));
}

bool writePrivateOutputEvent(acadctl::NativeOutputPort& port,
                             const resbuf* arguments) {
  if (!arguments || !arguments->rbnext || arguments->rbnext->rbnext ||
      (arguments->restype != RTSHORT && arguments->restype != RTLONG)) {
    return writeInvalidLispOutputEvent(port);
  }

  const int code = integerValue(arguments);
  const resbuf* payload = arguments->rbnext;
  acadctl::NativeLispOutputEvent event =
      lispOutputEvent(code, acadctl::NativeLispPayloadKind::Invalid);

  switch (payload->restype) {
  case RTNIL:
    event.payload_kind = acadctl::NativeLispPayloadKind::Nil;

    return writeLispOutputEvent(port, event);
  case RTSHORT:
  case RTLONG:
    event.payload_kind = acadctl::NativeLispPayloadKind::Integer;
    event.integer = integerValue(payload);

    return writeLispOutputEvent(port, event);
  case RTINT64:
    event.payload_kind = acadctl::NativeLispPayloadKind::Integer;
    event.integer = payload->resval.mnInt64;

    return writeLispOutputEvent(port, event);
  case RTREAL:
    event.payload_kind = acadctl::NativeLispPayloadKind::Real;
    event.real = payload->resval.rreal;

    return writeLispOutputEvent(port, event);
  case RTSTR:
    return writePrivateStringPayload(port, code, payload->resval.rstring);
  case RTENAME:
    return writePrivateEntityPayload(port, code, payload->resval.rlname);
  default:
    return writeLispOutputEvent(port, event);
  }
}

int acadctlOutputEvent() noexcept {
  try {
    bool keepGoing = false;
    ObjectArxBridge* bridge = ObjectArxBridge::commandBridge_;

    if (bridge && bridge->documentContextDispatch_ &&
        bridge->documentContextDispatch_->kind ==
            ObjectArxBridge::DocContextDispatch::Kind::ExecDriver &&
        bridge->documentContextDispatch_->phase ==
            ObjectArxBridge::DocContextDispatch::Phase::Running &&
        bridge->documentContextDispatch_->outputPort &&
        (bridge->documentContextDispatch_->stagedFormKind ==
             ObjectArxBridge::DocContextDispatch::StagedFormKind::Evaluator ||
         bridge->documentContextDispatch_->stagedFormKind ==
             ObjectArxBridge::DocContextDispatch::StagedFormKind::
                 EvalValueEmitter)) {
      ObjectArxBridge::DocContextDispatch& dispatch =
          *bridge->documentContextDispatch_;
      AcApDocument* document = bridge->document(dispatch.documentToken);

      if (document &&
          matchesExecutionContext(document, dispatch.databaseToken, document)) {
        keepGoing =
            writePrivateOutputEvent(**dispatch.outputPort, acedGetArgs());
      } else {
        acadctl::invalidate_output_port(**dispatch.outputPort);
      }
    }

    const int returnStatus = keepGoing ? acedRetT() : acedRetNil();

    if (returnStatus != RTNORM && bridge && bridge->documentContextDispatch_ &&
        bridge->documentContextDispatch_->outputPort) {
      acadctl::invalidate_output_port(
          **bridge->documentContextDispatch_->outputPort);
    }

    return returnStatus == RTNORM ? RSRSLT : RSERR;
  } catch (...) {
    ObjectArxBridge* bridge = ObjectArxBridge::commandBridge_;
    if (bridge && bridge->documentContextDispatch_ &&
        bridge->documentContextDispatch_->outputPort) {
      acadctl::invalidate_output_port(
          **bridge->documentContextDispatch_->outputPort);
    }

    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }
}

int defineLispFunction(const ACHAR* name, int code, int (*callback)()) {
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
  int status = defineLispFunction(
      kOutputEventFunction, kOutputEventFunctionCode, &acadctlOutputEvent);

  if (status != RTNORM) {
    undefineLispFunctions();

    return status;
  }

  status = defineLispFunction(kAdvanceExecutionFunction,
                              kAdvanceExecutionFunctionCode,
                              &acadctlAdvanceExecution);

  if (status != RTNORM) {
    undefineLispFunctions();

    return status;
  }

  return status;
}

int undefineLispFunctions() {
  const int executionStatus =
      acedUndef(kAdvanceExecutionFunction, kAdvanceExecutionFunctionCode);
  const int outputStatus =
      acedUndef(kOutputEventFunction, kOutputEventFunctionCode);

  if (executionStatus != RTNORM) {
    return executionStatus;
  }

  return outputStatus;
}

int putStringSymbol(const ACHAR* name, const ACHAR* text) {
  resbuf value{};
  value.restype = RTSTR;
  value.resval.rstring = const_cast<ACHAR*>(text);

  return acedPutSym(name, &value);
}

int putStringSymbol(const ACHAR* name, const AcString& text) {
  return putStringSymbol(name, text.kACharPtr());
}

int clearSymbol(const ACHAR* name) {
  resbuf value{};
  value.restype = RTNIL;

  return acedPutSym(name, &value);
}

ResbufPtr getSymbol(const ACHAR* name, int& status) {
  resbuf* value = nullptr;
  status = acedGetSym(name, &value);

  return ResbufPtr(value);
}

int integerValue(const resbuf* value) {
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

bool getIntegerSystemVariable(const ACHAR* name, int& value, int& status) {
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

#ifdef ACADCTL_HAS_ATIL
bool isRealisticVisualStyle() {
  const AcDbObjectId visualStyleId = acdbGetViewportVisualStyle();
  AcDbObjectPointer<AcDbVisualStyle> visualStyle(visualStyleId, AcDb::kForRead);
  if (visualStyle.openStatus() == Acad::eOk &&
      visualStyle->type() == AcGiVisualStyle::kRealistic) {
    return true;
  }

  resbuf result{};
  if (acedGetVar(ACRX_T("VSCURRENT"), &result) != RTNORM ||
      result.restype != RTSTR || !result.resval.rstring) {
    return false;
  }

  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(kRealisticVisualStyle);
  const bool realistic =
      std::char_traits<ACHAR>::length(result.resval.rstring) ==
          expectedLength &&
      std::char_traits<ACHAR>::compare(
          result.resval.rstring, kRealisticVisualStyle, expectedLength) == 0;
  acutDelString(result.resval.rstring);
  return realistic;
}
#endif

UndoGroupState observeUndoGroup(int& status) {
  int undoControl = 0;

  if (!getIntegerSystemVariable(ACRX_T("UNDOCTL"), undoControl, status)) {
    return UndoGroupState::Unknown;
  }

  return (undoControl & 8) != 0 ? UndoGroupState::Active
                                : UndoGroupState::Inactive;
}

int clearExecutionBridgeSymbols(bool includeValue = true) {
  int firstFailure = RTNORM;

  for (const ACHAR* name : {kSourceSymbol, kStagedFormSymbol, kStatusSymbol,
                            kErrorSymbol, kErrnoSymbol}) {
    const int status = clearSymbol(name);

    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }

  if (includeValue) {
    const int status = clearSymbol(kValueSymbol);

    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }

  return firstFailure;
}

acadctl::NativeBridgeStepResult
completeEvaluationCleanup(acadctl::NativeBridgeCleanupPlan plan,
                          int cleanupStatus) {
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
  const int cleanupStatus = clearExecutionBridgeSymbols(!plan.retain_value);

  return completeEvaluationCleanup(std::move(plan), cleanupStatus);
}

acadctl::NativeBridgeStepResult
finishEvaluation(acadctl::NativeExecStepResult result,
                 bool retainValueOnSuccess) {
  return finishEvaluation(
      acadctl::prepare_bridge_cleanup(std::move(result), retainValueOnSuccess));
}

acadctl::NativeBridgeStepResult stageEvaluation(rust::Str source,
                                                bool retainValue) {
  const int clearStatus = clearExecutionBridgeSymbols();

  if (clearStatus != RTNORM) {
    return completeEvaluationCleanup(
        acadctl::prepare_bridge_cleanup(stepNativeFailure(clearStatus), false),
        clearStatus);
  }

  {
    const AcString form(source.data(), AcString::Utf8,
                        static_cast<Adesk::UInt32>(source.size()));

    if (putStringSymbol(kSourceSymbol, form) != RTNORM ||
        putStringSymbol(kStagedFormSymbol, kEvalMarker) != RTNORM ||
        putStringSymbol(kStatusSymbol, kPendingStatus) != RTNORM) {
      return finishEvaluation(stepNativeFailure(RTERROR), retainValue);
    }
  }

  return {stepSuccess(), true};
}

acadctl::NativeLispObservation observeLispOutcome(int commandStatus) {
  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(kStatusSymbol, statusResult);
  const bool nilStatus =
      statusResult == RTNIL || (statusResult == RTNORM && !status) ||
      (statusResult == RTNORM && status && status->restype == RTNIL);
  const acadctl::NativeLispStatusKind statusKind =
      nilStatus                ? acadctl::NativeLispStatusKind::Nil
      : statusResult != RTNORM ? acadctl::NativeLispStatusKind::Unavailable
      : status && status->restype == RTT ? acadctl::NativeLispStatusKind::True
                                         : acadctl::NativeLispStatusKind::Other;

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(kErrnoSymbol, errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;

  int errorResult = RTERROR;
  ResbufPtr error = getSymbol(kErrorSymbol, errorResult);
  BoundedNativeText detail{rust::String(), false};
  const bool errorAvailable = errorResult == RTNORM && error &&
                              error->restype == RTSTR && error->resval.rstring;

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

acadctl::NativeBridgeCleanupPlan outputEmitterOutcome(int commandStatus) {
  return acadctl::interpret_lisp_observation(observeLispOutcome(commandStatus),
                                             false);
}

struct UndoCommandResult {
  acadctl::NativeExecStepResult result;
  UndoGroupState state;
};

UndoCommandResult runUndoCommand(const ACHAR* option,
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
  if (!executionUndoGroupMayHaveStarted || state == UndoGroupState::Unknown) {
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

  if (end.result.kind != acadctl::NativeExecStepResultKind::Success) {
    return {std::move(end.result), finalState};
  }

  return {stepSuccess(), finalState};
}

bool matchesDatabase(AcApDocument* document, std::size_t databaseToken) {
  return document && static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
                         document->database())) == databaseToken;
}

bool matchesExecutionContext(AcApDocument* document, std::size_t databaseToken,
                             AcApDocument* expectedActive) {
  return matchesDatabase(document, databaseToken) &&
         acDocManager->curDocument() == document &&
         acDocManager->mdiActiveDocument() == expectedActive;
}

int clearExecutionBridgeSymbolsIfSafe(AcApDocument* document,
                                      std::size_t databaseToken,
                                      AcApDocument* expectedActive,
                                      bool& bridgeSymbolsMayBeRetained) {
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
    std::uint64_t jobId, AcApDocument* document, std::size_t databaseToken,
    AcApDocument* expectedActive, bool& bridgeSymbolsMayBeRetained) {
  const int cleanupStatus = clearExecutionBridgeSymbolsIfSafe(
      document, databaseToken, expectedActive, bridgeSymbolsMayBeRetained);

  if (!acadctl::abandon_execution(
          jobId, stepNativeFailure(cleanupStatus == RTNORM ? RTERROR
                                                           : cleanupStatus))) {
    return bridgeFailure(acadctl::NativeActionResultKind::ExecBridgeFailed,
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
    acadctl::native_action_wake_failed();
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

ObjectArxBridge* ObjectArxBridge::commandBridge_ = nullptr;

Acad::ErrorStatus ObjectArxBridge::start() {
  const Acad::ErrorStatus commandStatus = acedRegCmds->addCommand(
      kHistoryCommandGroup, kHistoryCommandName, kHistoryCommandName,
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
    if (AcApDocument* document = iterator->document()) {
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

  for (DocSubscription& subscription : subscriptions_) {
    detachDatabaseReactor(subscription);
  }

  subscriptions_.clear();
  databaseReactors_.erase(std::remove_if(databaseReactors_.begin(),
                                         databaseReactors_.end(),
                                         [](const auto& uncertain) {
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
  rust::Box<acadctl::NativeAction> action = acadctl::take_native_action();

  if (action->kind() == acadctl::NativeActionKind::None) {
    scheduleNextNativeAction();

    return;
  }

  acadctl::NativeActionResult actionResult =
      result(acadctl::NativeActionResultKind::Success);

  switch (action->kind()) {
  case acadctl::NativeActionKind::Open:
    actionResult = open(action->open_path());
    break;
  case acadctl::NativeActionKind::Switch:
    if (AcApDocument* target = document(action->document_token())) {
      actionResult =
          matchesDatabase(target, action->database_token())
              ? switchTo(target)
              : result(
                    acadctl::NativeActionResultKind::DrawingGenerationChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DrawingGone);
    }

    break;
  case acadctl::NativeActionKind::Save:
    if (AcApDocument* target = document(action->document_token())) {
      actionResult =
          matchesDatabase(target, action->database_token())
              ? save(target, action->save_path())
              : result(
                    acadctl::NativeActionResultKind::DrawingGenerationChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DrawingGone);
    }

    break;
  case acadctl::NativeActionKind::Close:
    if (AcApDocument* target = document(action->document_token())) {
      actionResult =
          matchesDatabase(target, action->database_token())
              ? close(target, action->close_discard())
              : result(
                    acadctl::NativeActionResultKind::DrawingGenerationChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DrawingGone);
    }

    break;
  case acadctl::NativeActionKind::Capture: {
    ViewportCaptureResult captured =
        captureResult(acadctl::NativeCaptureResultKind::DrawingGone);
    if (AcApDocument* target = document(action->document_token())) {
      captured =
          matchesDatabase(target, action->database_token())
              ? capture(target)
              : captureResult(
                    acadctl::NativeCaptureResultKind::DrawingGenerationChanged);
    }

    refreshDocumentSnapshot();
    const rust::Slice<const std::uint8_t> pixels(captured.pixels.data(),
                                                 captured.pixels.size());
    acadctl::complete_native_capture(action->job_id(),
                                     std::move(captured.metadata), pixels);
    scheduleNextNativeAction();
    return;
  }
  case acadctl::NativeActionKind::Undo:
  case acadctl::NativeActionKind::Redo:
  case acadctl::NativeActionKind::QueueExecDriver:
    if (queueDocumentContextDispatch(*action, actionResult)) {
      return;
    }

    break;
  case acadctl::NativeActionKind::None:
    return;
  }

  refreshDocumentSnapshot();
  acadctl::complete_native_action(action->job_id(), std::move(actionResult));
  scheduleNextNativeAction();
}

void ObjectArxBridge::setLispFunctionsDefined(AcApDocument* document,
                                              bool defined) {
  if (!document) {
    return;
  }

  if (defined) {
    subscribe(document);
  }

  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocSubscription& candidate) {
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

AcApDocument* ObjectArxBridge::document(std::size_t token) {
  const auto subscription = std::find_if(
      subscriptions_.begin(), subscriptions_.end(),
      [token](const DocSubscription& candidate) {
        return static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
                   candidate.document)) == token;
      });

  return subscription == subscriptions_.end() ? nullptr
                                              : subscription->document;
}

bool ObjectArxBridge::lispFunctionsDefined(AcApDocument* document) const {
  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocSubscription& candidate) {
                     return candidate.document == document;
                   });

  return subscription != subscriptions_.end() &&
         subscription->lispFunctionsDefined;
}

bool ObjectArxBridge::applicationContextBlocked(AcApDocument* target) const {
  AcApDocument* active = acDocManager->mdiActiveDocument();

  if (active &&
      (acDocManager->curDocument() != active || !active->isQuiescent() ||
       acDocManager->inputPending(active) > 0)) {
    return true;
  }

  return target && target != active &&
         (!target->isQuiescent() || acDocManager->inputPending(target) > 0);
}

ViewportCaptureResult ObjectArxBridge::capture(AcApDocument* document) {
  if (applicationContextBlocked(document)) {
    return captureResult(acadctl::NativeCaptureResultKind::NotQuiescent);
  }

  if (document != acDocManager->mdiActiveDocument()) {
    return captureResult(acadctl::NativeCaptureResultKind::NotActive);
  }

  int viewport = 0;
  int viewportStatus = RTERROR;
  if (!getIntegerSystemVariable(ACRX_T("CVPORT"), viewport, viewportStatus) ||
      viewport <= 0) {
    return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                         "the active viewport number is unavailable");
  }

  AcGsView* const view = acgsGetCurrent3dAcGsView(viewport);
  if (view) {
#ifdef ACADCTL_HAS_ATIL
    int left = 0;
    int bottom = 0;
    int right = 0;
    int top = 0;
    if (!acgsGetViewportInfo(viewport, left, bottom, right, top)) {
      return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                           "the active viewport bounds are unavailable");
    }

    const std::int64_t widthValue =
        static_cast<std::int64_t>(right) - static_cast<std::int64_t>(left);
    const std::int64_t heightValue =
        static_cast<std::int64_t>(top) - static_cast<std::int64_t>(bottom);
    if (widthValue <= 0 || heightValue <= 0) {
      return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                           "the active viewport dimensions are invalid");
    }
    if (widthValue > kMaximumCaptureDimension ||
        heightValue > kMaximumCaptureDimension) {
      return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                           "the active viewport is too large; resize the "
                           "AutoCAD window and retry");
    }

    const int width = static_cast<int>(widthValue);
    const int height = static_cast<int>(heightValue);
    const int stride =
        Atil::DataModel::bytesPerRow(width, Atil::DataModelAttributes::k32);
    if (stride < width * 4) {
      return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                           "ATIL returned an invalid capture stride");
    }

    const std::size_t byteCount =
        static_cast<std::size_t>(stride) * static_cast<std::size_t>(height);
    if (byteCount == 0 ||
        byteCount > static_cast<std::size_t>(std::numeric_limits<int>::max())) {
      return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                           "the active viewport capture dimensions overflow");
    }
    if (byteCount > kMaximumCaptureBytes) {
      return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                           "the active viewport is too large; resize the "
                           "AutoCAD window and retry");
    }

    ViewportCaptureResult captured =
        captureResult(acadctl::NativeCaptureResultKind::Success);
    try {
      captured.pixels.resize(byteCount);
      Atil::RgbModel model(Atil::RgbModelAttributes::k4Channels,
                           Atil::DataModelAttributes::kBlueGreenRedAlpha);
      const Atil::Size size(width, height);
      Atil::Image image(captured.pixels.data(), static_cast<int>(byteCount),
                        stride, size, &model);
      view->getSnapShot(&image, AcGsDCPoint(0, 0));
    } catch (...) {
      return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                           "ATIL could not capture the active viewport");
    }

    captured.metadata.width = static_cast<std::uint32_t>(width);
    captured.metadata.height = static_cast<std::uint32_t>(height);
    captured.metadata.stride = static_cast<std::size_t>(stride);
    captured.metadata.pixel_format = acadctl::NativePixelFormat::Bgra8;
    captured.metadata.row_order = acadctl::NativeRowOrder::BottomUp;
    captured.metadata.realistic_style = isRealisticVisualStyle();
    return captured;
#else
    return captureResult(
        acadctl::NativeCaptureResultKind::Unavailable,
        "3D viewport capture requires official ATIL headers at build time");
#endif
  }

  std::unique_ptr<AcGsScreenShot> screenShot(acgsGetScreenShot(viewport));
  if (!screenShot) {
    return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                         "AutoCAD did not provide a 2D viewport capture");
  }

  int width = 0;
  int height = 0;
  int depth = 0;
  if (!screenShot->getSize(width, height, depth)) {
    return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                         "AutoCAD returned invalid 2D capture metadata");
  }

  if (width <= 0 || height <= 0 || depth != 32) {
    return captureResult(
        acadctl::NativeCaptureResultKind::Invalid,
        "the 2D capture dimensions or pixel depth are invalid");
  }
  if (width > kMaximumCaptureDimension || height > kMaximumCaptureDimension) {
    return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                         "the active viewport is too large; resize the AutoCAD "
                         "window and retry");
  }

  const std::size_t stride = static_cast<std::size_t>(width) * 4;
  const std::size_t byteCount = stride * static_cast<std::size_t>(height);
  if (byteCount == 0) {
    return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                         "the 2D viewport capture dimensions overflow");
  }
  if (byteCount > kMaximumCaptureBytes) {
    return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                         "the active viewport is too large; resize the AutoCAD "
                         "window and retry");
  }

  ViewportCaptureResult captured =
      captureResult(acadctl::NativeCaptureResultKind::Success);
  try {
    captured.pixels.resize(byteCount);
  } catch (...) {
    return captureResult(acadctl::NativeCaptureResultKind::Unavailable,
                         "memory for the 2D viewport capture is unavailable");
  }

  for (int row = 0; row < height; ++row) {
    const void* const scanline = screenShot->getScanline(0, row);
    if (!scanline) {
      return captureResult(acadctl::NativeCaptureResultKind::Invalid,
                           "AutoCAD returned an invalid 2D capture scanline");
    }

    std::memcpy(captured.pixels.data() + static_cast<std::size_t>(row) * stride,
                scanline, stride);
  }

  captured.metadata.width = static_cast<std::uint32_t>(width);
  captured.metadata.height = static_cast<std::uint32_t>(height);
  captured.metadata.stride = stride;
  captured.metadata.pixel_format = acadctl::NativePixelFormat::Bgrx8;
  captured.metadata.row_order = acadctl::NativeRowOrder::TopDown;
  return captured;
}

acadctl::NativeActionResult ObjectArxBridge::open(rust::Str path) {
  if (applicationContextBlocked()) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }

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

acadctl::NativeActionResult ObjectArxBridge::save(AcApDocument* document,
                                                  rust::Str path) {
  const bool saveAs = !path.empty();

  if (!saveAs && !document->isNamedDrawing()) {
    return result(acadctl::NativeActionResultKind::Unnamed);
  }

  if (document->isReadOnly()) {
    return result(acadctl::NativeActionResultKind::ReadOnly);
  }

  if (saveAs) {
    std::error_code filesystemError;
    const std::filesystem::path filesystemPath =
        std::filesystem::u8path(path.data(), path.data() + path.size());
    const std::filesystem::file_status destinationStatus =
        std::filesystem::symlink_status(filesystemPath, filesystemError);

    if (destinationStatus.type() != std::filesystem::file_type::not_found) {
      return result(acadctl::NativeActionResultKind::DestinationExists);
    }

    if (filesystemError &&
        filesystemError != std::errc::no_such_file_or_directory) {
      return nativeFailure(acadctl::NativeActionResultKind::SaveFailed,
                           Acad::eFileSystemErr);
    }
  }

  const AcString destination =
      saveAs ? AcString(path.data(), AcString::Utf8,
                        static_cast<Adesk::UInt32>(path.size()))
             : AcString(document->fileName());

  if (applicationContextBlocked(document)) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }

  const Acad::ErrorStatus lockStatus = acDocManager->lockDocument(
      document, AcAp::kXWrite, nullptr, nullptr, false);

  if (lockStatus != Acad::eOk) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }

  AcApDocument* active = acDocManager->mdiActiveDocument();
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
          document->database()->saveAs(destination.constPtr(), true, version);
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

acadctl::NativeActionResult ObjectArxBridge::switchTo(AcApDocument* document) {
  if (applicationContextBlocked(document)) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }

  if (acDocManager->mdiActiveDocument() == document &&
      acDocManager->curDocument() == document) {
    return result(acadctl::NativeActionResultKind::Success);
  }

  const Acad::ErrorStatus status =
      acDocManager->activateDocument(document, false);

  if (status == Acad::eOk && (acDocManager->mdiActiveDocument() != document ||
                              acDocManager->curDocument() != document)) {
    return nativeFailure(acadctl::NativeActionResultKind::SwitchFailed,
                         Acad::eInvalidContext);
  }

  return status == Acad::eOk
             ? result(acadctl::NativeActionResultKind::Success)
             : nativeFailure(acadctl::NativeActionResultKind::SwitchFailed,
                             status);
}

acadctl::NativeActionResult ObjectArxBridge::close(AcApDocument* document,
                                                   bool discard) {
  AcDbDatabase* database = document->database();
  const int dbmod = acdbGetDbmod(database);

  if (dbmod != 0 && !discard) {
    return result(acadctl::NativeActionResultKind::Dirty);
  }

  if (applicationContextBlocked(document)) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
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
    const acadctl::NativeAction& action, acadctl::NativeActionResult& failure) {
  if (documentContextDispatch_) {
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, RTERROR);

    return false;
  }

  DocContextDispatch::Kind kind;

  switch (action.kind()) {
  case acadctl::NativeActionKind::Undo:
    kind = DocContextDispatch::Kind::Undo;
    break;
  case acadctl::NativeActionKind::Redo:
    kind = DocContextDispatch::Kind::Redo;
    break;
  case acadctl::NativeActionKind::QueueExecDriver:
    kind = DocContextDispatch::Kind::ExecDriver;
    break;
  default:
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, RTERROR);

    return false;
  }

  AcApDocument* target = document(action.document_token());

  if (!target) {
    failure = result(acadctl::NativeActionResultKind::DrawingGone);

    return false;
  }

  if (!matchesDatabase(target, action.database_token())) {
    failure = result(acadctl::NativeActionResultKind::DrawingGenerationChanged);

    return false;
  }

  AcApDocument* previousActive = acDocManager->mdiActiveDocument();

  if (!previousActive) {
    failure =
        nativeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
                      Acad::eNoDocument);

    return false;
  }

  const bool forceDocumentContext = action.force_document_context();

  if (target != previousActive && !forceDocumentContext) {
    failure = result(acadctl::NativeActionResultKind::NotActive);

    return false;
  }

  if (kind == DocContextDispatch::Kind::ExecDriver) {
    if (!lispFunctionsDefined(target)) {
      failure = bridgeFailure(acadctl::NativeActionResultKind::ExecBridgeFailed,
                              RTERROR);

      return false;
    }

    if (!target->database()->undoRecording()) {
      failure = result(acadctl::NativeActionResultKind::UndoDisabled);

      return false;
    }
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

  const bool restorePreviousActive =
      forceDocumentContext && previousActive != target;
  const int pendingInput = acDocManager->inputPending(target);

  if (pendingInput > 0) {
    failure = result(acadctl::NativeActionResultKind::NotQuiescent);

    return false;
  }

  if (pendingInput < 0) {
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, RTERROR);

    return false;
  }

  const std::size_t previousActiveToken = static_cast<std::size_t>(
      reinterpret_cast<std::uintptr_t>(previousActive));
  const std::size_t previousActiveDatabaseToken = static_cast<std::size_t>(
      reinterpret_cast<std::uintptr_t>(previousActive->database()));

  documentContextDispatch_.emplace(DocContextDispatch{
      action.job_id(),
      action.document_token(),
      action.database_token(),
      previousActiveToken,
      previousActiveDatabaseToken,
      kind,
      restorePreviousActive,
      result(acadctl::NativeActionResultKind::Success),
      DocContextDispatch::Phase::Queued,
  });

  nativeActionCallbacksOutstanding.fetch_add(1, std::memory_order_seq_cst);
  const ACHAR* invocation = kind == DocContextDispatch::Kind::ExecDriver
                                ? kExecutionDriverInvocation
                                : kHistoryCommandInvocation;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->sendStringToExecute(target, invocation, true, false, false);

  if (scheduleStatus == Acad::eOk) {
    return true;
  }

  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  documentContextDispatch_.reset();
  failure = nativeFailure(
      acadctl::NativeActionResultKind::DocumentContextFailed, scheduleStatus);

  Acad::ErrorStatus restoreStatus = Acad::eOk;
  AcApDocument* restorablePreviousActive = document(previousActiveToken);

  if (restorePreviousActive &&
      !matchesDatabase(restorablePreviousActive, previousActiveDatabaseToken)) {
    restoreStatus = Acad::eNoDocument;
  } else if (restorePreviousActive &&
             acDocManager->mdiActiveDocument() != restorablePreviousActive) {
    restoreStatus =
        acDocManager->activateDocument(restorablePreviousActive, false);
  }

  if (restoreStatus != Acad::eOk ||
      acDocManager->mdiActiveDocument() != restorablePreviousActive ||
      acDocManager->curDocument() != restorablePreviousActive) {
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

  DocContextDispatch& dispatch = *documentContextDispatch_;
  dispatch.phase = DocContextDispatch::Phase::Finalizing;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->beginExecuteInApplicationContext(
          finalizeDocumentContextDispatch, nullptr);

  if (scheduleStatus == Acad::eOk) {
    return;
  }

  dispatch.dispatchResult = nativeFailure(
      acadctl::NativeActionResultKind::DocumentContextRestoreFailed,
      scheduleStatus);
  const std::uint64_t jobId = dispatch.jobId;
  acadctl::NativeActionResult dispatchResult =
      std::move(dispatch.dispatchResult);
  const bool executionDriver =
      dispatch.kind == DocContextDispatch::Kind::ExecDriver;
  const acadctl::NativeExecFinalizationObservation observation =
      finalizationObservation(dispatch);
  documentContextDispatch_.reset();
  if (executionDriver) {
    acadctl::complete_execution_native_action(jobId, std::move(dispatchResult),
                                              observation);
  } else {
    acadctl::complete_native_action(jobId, std::move(dispatchResult));
  }
  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
}

void ObjectArxBridge::queuedHistoryCommandTerminated(const ACHAR* commandName) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->phase != DocContextDispatch::Phase::Queued ||
      !commandName) {
    return;
  }

  if (documentContextDispatch_->kind == DocContextDispatch::Kind::ExecDriver) {
    return;
  }

  const std::size_t commandLength =
      std::char_traits<ACHAR>::length(commandName);
  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(kHistoryCommandName);

  if (commandLength != expectedLength ||
      std::char_traits<ACHAR>::compare(commandName, kHistoryCommandName,
                                       expectedLength) != 0) {
    return;
  }

  documentContextDispatch_->dispatchResult =
      bridgeFailure(acadctl::NativeActionResultKind::HistoryFailed, RTERROR);
  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::queuedExecutionDriverStarted(const ACHAR* firstLine) {
  if (!documentContextDispatch_ ||
      (documentContextDispatch_->phase != DocContextDispatch::Phase::Queued &&
       documentContextDispatch_->phase != DocContextDispatch::Phase::Running) ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver ||
      !firstLine) {
    return;
  }

  DocContextDispatch& dispatch = *documentContextDispatch_;

  if (dispatch.driverLifecycle !=
      DocContextDispatch::ExecDriverLifecycle::AwaitingStart) {
    if (dispatch.driverLifecycle ==
            DocContextDispatch::ExecDriverLifecycle::EndedDuringCallback ||
        dispatch.driverLifecycle ==
            DocContextDispatch::ExecDriverLifecycle::Finalizing) {
      return;
    }

    if (dispatch.lispDepth == std::numeric_limits<std::uint32_t>::max()) {
      failExecutionDriver();

      return;
    }

    ++dispatch.lispDepth;

    return;
  }

  const std::size_t actualLength = std::char_traits<ACHAR>::length(firstLine);
  const ACHAR* driverExpression = kExecutionDriverExpression;
  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(driverExpression);

  if (actualLength != expectedLength ||
      std::char_traits<ACHAR>::compare(firstLine, driverExpression,
                                       expectedLength) != 0) {
    return;
  }

  dispatch.driverLifecycle = DocContextDispatch::ExecDriverLifecycle::Running;
  dispatch.lispDepth = 1;
}

void ObjectArxBridge::failExecutionDriver() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver ||
      documentContextDispatch_->phase ==
          DocContextDispatch::Phase::Finalizing) {
    return;
  }

  DocContextDispatch& dispatch = *documentContextDispatch_;
  finishOutputPort(dispatch.outputPort, false);

  dispatch.dispatchResult =
      bridgeFailure(acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
  scheduleExecutionDispatchFinalizer();
}

void ObjectArxBridge::recoverCancelledExecutionDriver() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver ||
      documentContextDispatch_->phase ==
          DocContextDispatch::Phase::Finalizing) {
    return;
  }

  DocContextDispatch& dispatch = *documentContextDispatch_;
  finishOutputPort(dispatch.outputPort, true);

  AcApDocument* target = document(dispatch.documentToken);

  if (!target ||
      !matchesExecutionContext(target, dispatch.databaseToken, target)) {
    dispatch.bridgeSymbolsMayBeRetained =
        dispatch.bridgeSymbolsMayBeRetained ||
        dispatch.stagedFormKind != DocContextDispatch::StagedFormKind::None;
    dispatch.dispatchResult = abandonLostExecutionContext(
        dispatch.jobId, target, dispatch.databaseToken, target,
        dispatch.bridgeSymbolsMayBeRetained);
    scheduleExecutionDispatchFinalizer();

    return;
  }

  bool interruptedStepRecorded = false;

  if (dispatch.stagedFormKind ==
      DocContextDispatch::StagedFormKind::Evaluator) {
    acadctl::NativeBridgeStepResult interrupted =
        finishEvaluation(stepNativeFailure(RTERROR), false);
    dispatch.bridgeSymbolsMayBeRetained =
        interrupted.bridge_symbols_may_be_retained;
    dispatch.stagedFormKind = DocContextDispatch::StagedFormKind::None;
    interruptedStepRecorded = acadctl::complete_execution_step(
        dispatch.jobId, std::move(interrupted.result));
  } else if (dispatch.stagedFormKind ==
             DocContextDispatch::StagedFormKind::EvalValueEmitter) {
    acadctl::NativeBridgeStepResult interrupted =
        finishEvaluation(stepNativeFailure(RTERROR), false);
    dispatch.bridgeSymbolsMayBeRetained =
        interrupted.bridge_symbols_may_be_retained;
    dispatch.stagedFormKind = DocContextDispatch::StagedFormKind::None;
    interruptedStepRecorded = acadctl::complete_execution_step(
        dispatch.jobId, std::move(interrupted.result));
  }

  if (!interruptedStepRecorded) {
    interruptedStepRecorded =
        acadctl::abandon_execution(dispatch.jobId, stepNativeFailure(RTERROR));
  }

  if (!interruptedStepRecorded) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
    scheduleExecutionDispatchFinalizer();

    return;
  }

  dispatch.phase = DocContextDispatch::Phase::Queued;
  dispatch.driverLifecycle =
      DocContextDispatch::ExecDriverLifecycle::AwaitingStart;
  dispatch.lispDepth = 0;
  const Acad::ErrorStatus scheduleStatus = acDocManager->sendStringToExecute(
      target, kExecutionDriverInvocation, true, false, false);

  if (scheduleStatus != Acad::eOk) {
    dispatch.dispatchResult = nativeFailure(
        acadctl::NativeActionResultKind::DocumentContextFailed, scheduleStatus);
    scheduleExecutionDispatchFinalizer();
  }
}

void ObjectArxBridge::scheduleExecutionDispatchFinalizer() {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver) {
    return;
  }

  documentContextDispatch_->driverLifecycle =
      DocContextDispatch::ExecDriverLifecycle::Finalizing;
  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::queuedExecutionDriverTerminated(bool cancelled) {
  if (!documentContextDispatch_ ||
      (documentContextDispatch_->phase != DocContextDispatch::Phase::Queued &&
       documentContextDispatch_->phase != DocContextDispatch::Phase::Running) ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver ||
      documentContextDispatch_->driverLifecycle ==
          DocContextDispatch::ExecDriverLifecycle::AwaitingStart ||
      documentContextDispatch_->driverLifecycle ==
          DocContextDispatch::ExecDriverLifecycle::Finalizing) {
    return;
  }

  DocContextDispatch& dispatch = *documentContextDispatch_;

  if (cancelled) {
    recoverCancelledExecutionDriver();

    return;
  }

  if (dispatch.lispDepth > 1) {
    --dispatch.lispDepth;

    return;
  }

  dispatch.lispDepth = 0;

  if (dispatch.driverLifecycle ==
      DocContextDispatch::ExecDriverLifecycle::InCallback) {
    dispatch.driverLifecycle =
        DocContextDispatch::ExecDriverLifecycle::EndedDuringCallback;
  } else if (dispatch.driverLifecycle ==
             DocContextDispatch::ExecDriverLifecycle::AwaitingEnd) {
    scheduleExecutionDispatchFinalizer();
  } else {
    failExecutionDriver();
  }
}

void ObjectArxBridge::finishAdvanceCallback(AdvanceCompletion completion) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->kind != DocContextDispatch::Kind::ExecDriver ||
      documentContextDispatch_->phase ==
          DocContextDispatch::Phase::Finalizing) {
    return;
  }

  DocContextDispatch& dispatch = *documentContextDispatch_;
  if (dispatch.driverLifecycle ==
      DocContextDispatch::ExecDriverLifecycle::EndedDuringCallback) {
    if (completion == AdvanceCompletion::ExitReady) {
      scheduleExecutionDispatchFinalizer();
    } else {
      failExecutionDriver();
    }
  } else if (dispatch.driverLifecycle ==
             DocContextDispatch::ExecDriverLifecycle::InCallback) {
    dispatch.driverLifecycle =
        completion == AdvanceCompletion::ExitReady
            ? DocContextDispatch::ExecDriverLifecycle::AwaitingEnd
            : DocContextDispatch::ExecDriverLifecycle::Running;
  } else {
    failExecutionDriver();
  }
}

int acadctlAdvanceExecution() noexcept {
  ObjectArxBridge* bridge = ObjectArxBridge::commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      (bridge->documentContextDispatch_->phase !=
           ObjectArxBridge::DocContextDispatch::Phase::Queued &&
       bridge->documentContextDispatch_->phase !=
           ObjectArxBridge::DocContextDispatch::Phase::Running) ||
      bridge->documentContextDispatch_->kind !=
          ObjectArxBridge::DocContextDispatch::Kind::ExecDriver) {
    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }

  ObjectArxBridge::DocContextDispatch& dispatch =
      *bridge->documentContextDispatch_;

  if (dispatch.driverLifecycle ==
      ObjectArxBridge::DocContextDispatch::ExecDriverLifecycle::AwaitingStart) {
    dispatch.driverLifecycle =
        ObjectArxBridge::DocContextDispatch::ExecDriverLifecycle::Running;
    dispatch.lispDepth = 1;
  }

  if (dispatch.driverLifecycle !=
      ObjectArxBridge::DocContextDispatch::ExecDriverLifecycle::Running) {
    bridge->failExecutionDriver();

    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }

  dispatch.phase = ObjectArxBridge::DocContextDispatch::Phase::Running;
  dispatch.driverLifecycle =
      ObjectArxBridge::DocContextDispatch::ExecDriverLifecycle::InCallback;
  bool evaluateStagedForm = false;
  try {
    AcApDocument* target = bridge->document(dispatch.documentToken);

    if (!target) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::DrawingGone);
    } else if (!matchesDatabase(target, dispatch.databaseToken)) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::DrawingGenerationChanged);
    } else if (acDocManager->mdiActiveDocument() != target ||
               acDocManager->curDocument() != target) {
      dispatch.dispatchResult =
          nativeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
                        Acad::eInvalidContext);
    } else if (!bridge->lispFunctionsDefined(target)) {
      dispatch.dispatchResult = bridgeFailure(
          acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
    } else if (!target->database()->undoRecording()) {
      dispatch.dispatchResult =
          result(acadctl::NativeActionResultKind::UndoDisabled);
    } else {
      if (dispatch.stagedFormKind ==
          ObjectArxBridge::DocContextDispatch::StagedFormKind::Evaluator) {
        finishOutputPort(dispatch.outputPort, false);
        acadctl::NativeBridgeStepResult evaluation =
            finishEvaluation(collectEvaluation(dispatch.retainValue));
        dispatch.bridgeSymbolsMayBeRetained =
            evaluation.bridge_symbols_may_be_retained;
        dispatch.stagedFormKind =
            ObjectArxBridge::DocContextDispatch::StagedFormKind::None;

        int observationStatus = RTERROR;
        dispatch.undoGroup = observeUndoGroup(observationStatus);

        if (evaluation.result.kind ==
                acadctl::NativeExecStepResultKind::Success &&
            dispatch.undoGroup != UndoGroupState::Active) {
          evaluation.result = stepNativeFailure(
              dispatch.undoGroup == UndoGroupState::Unknown ? observationStatus
                                                            : RTERROR);
        }

        if (!acadctl::complete_execution_step(dispatch.jobId,
                                              std::move(evaluation.result))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
        }
      } else if (dispatch.stagedFormKind ==
                 ObjectArxBridge::DocContextDispatch::StagedFormKind::
                     EvalValueEmitter) {
        acadctl::NativeBridgeStepResult emission =
            finishEvaluation(outputEmitterOutcome(RTNORM));
        dispatch.bridgeSymbolsMayBeRetained =
            emission.bridge_symbols_may_be_retained;
        dispatch.stagedFormKind =
            ObjectArxBridge::DocContextDispatch::StagedFormKind::None;
        finishOutputPort(dispatch.outputPort, false);

        if (!acadctl::complete_execution_step(dispatch.jobId,
                                              std::move(emission.result))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
        }
      }

      while (dispatch.dispatchResult.kind ==
             acadctl::NativeActionResultKind::Success) {
        if (!matchesExecutionContext(target, dispatch.databaseToken, target)) {
          dispatch.dispatchResult = abandonLostExecutionContext(
              dispatch.jobId, target, dispatch.databaseToken, target,
              dispatch.bridgeSymbolsMayBeRetained);
          break;
        }

        rust::Box<acadctl::NativeExecStep> step =
            acadctl::take_execution_step(dispatch.jobId);
        const acadctl::NativeExecStepKind kind =
            acadctl::execution_step_kind(*step);

        if (kind == acadctl::NativeExecStepKind::Done) {
          if (dispatch.undoGroup != UndoGroupState::Inactive) {
            UndoCommandResult cleanup =
                dispatch.formHandedOff
                    ? rollbackUndoGroup(
                          dispatch.undoGroup,
                          dispatch.executionUndoGroupMayHaveStarted)
                    : runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
            dispatch.undoGroup = cleanup.state;

            if (cleanup.result.kind !=
                    acadctl::NativeExecStepResultKind::Success ||
                dispatch.undoGroup != UndoGroupState::Inactive) {
              dispatch.terminalCleanupFailed = true;
              dispatch.dispatchResult = bridgeFailure(
                  acadctl::NativeActionResultKind::ExecBridgeFailed,
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
                  acadctl::NativeActionResultKind::ExecBridgeSymbolsClearFailed,
                  cleanupStatus);
            }
          }

          break;
        }

        if (kind == acadctl::NativeExecStepKind::Invalid) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
          break;
        }

        acadctl::NativeExecStepResult stepResult = stepSuccess();
        UndoCommandResult undoTransition{stepSuccess(), dispatch.undoGroup};

        switch (kind) {
        case acadctl::NativeExecStepKind::BeginUndoGroup:
          undoTransition =
              runUndoCommand(ACRX_T("_Begin"), UndoGroupState::Active);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          dispatch.executionUndoGroupMayHaveStarted =
              dispatch.undoGroup != UndoGroupState::Inactive;
          break;
        case acadctl::NativeExecStepKind::EvaluateForm: {
          dispatch.formHandedOff = true;
          const bool retainValue = acadctl::execution_step_retain_value(*step);
          rust::Box<acadctl::NativeOutputPort> port =
              acadctl::begin_form_output(dispatch.jobId, dispatch.documentToken,
                                         dispatch.databaseToken);

          if (!acadctl::output_port_claimed(*port) || dispatch.outputPort) {
            acadctl::invalidate_output_port(*port);
            acadctl::finish_output_port(std::move(port));
            stepResult = stepNativeFailure(RTERROR);
            break;
          }

          acadctl::NativeBridgeStepResult staging = stageEvaluation(
              acadctl::execution_step_source(*step), retainValue);
          dispatch.bridgeSymbolsMayBeRetained =
              staging.bridge_symbols_may_be_retained;

          if (staging.result.kind ==
              acadctl::NativeExecStepResultKind::Success) {
            dispatch.stagedFormKind =
                ObjectArxBridge::DocContextDispatch::StagedFormKind::Evaluator;
            dispatch.retainValue = retainValue;
            dispatch.outputPort.emplace(std::move(port));
            evaluateStagedForm = true;
            break;
          }

          acadctl::finish_output_port(std::move(port));
          stepResult = std::move(staging.result);
          break;
        }

        case acadctl::NativeExecStepKind::CommitUndoGroup:
        case acadctl::NativeExecStepKind::CloseEmptyUndoGroup:
          undoTransition =
              runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecStepKind::RollbackUndoGroup:
          undoTransition = rollbackUndoGroup(
              dispatch.undoGroup, dispatch.executionUndoGroupMayHaveStarted);
          stepResult = std::move(undoTransition.result);
          dispatch.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecStepKind::ClearRetainedEvalValue: {
          const int cleanupStatus = clearExecutionBridgeSymbols();
          stepResult = cleanupStatus == RTNORM
                           ? stepSuccess()
                           : stepNativeFailure(cleanupStatus);
          dispatch.bridgeSymbolsMayBeRetained = cleanupStatus != RTNORM;
          break;
        }

        case acadctl::NativeExecStepKind::EmitEvalValue: {
          rust::Box<acadctl::NativeOutputPort> port =
              acadctl::begin_eval_output(dispatch.jobId, dispatch.documentToken,
                                         dispatch.databaseToken);

          if (!acadctl::output_port_claimed(*port) || dispatch.outputPort) {
            acadctl::invalidate_output_port(*port);
            acadctl::finish_output_port(std::move(port));
            acadctl::NativeBridgeStepResult emission =
                finishEvaluation(stepSuccess(), false);
            stepResult = std::move(emission.result);
            dispatch.bridgeSymbolsMayBeRetained =
                emission.bridge_symbols_may_be_retained;
            break;
          }

          int preparationStatus = clearExecutionBridgeSymbols(false);

          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(kStagedFormSymbol,
                                                kEmitRetainedValueExpression);
          }

          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(kStatusSymbol, kPendingStatus);
          }

          if (preparationStatus != RTNORM) {
            acadctl::invalidate_output_port(*port);
            acadctl::finish_output_port(std::move(port));
            acadctl::NativeBridgeStepResult emission =
                finishEvaluation(stepNativeFailure(preparationStatus), false);
            stepResult = std::move(emission.result);
            dispatch.bridgeSymbolsMayBeRetained =
                emission.bridge_symbols_may_be_retained;
            break;
          }

          dispatch.outputPort.emplace(std::move(port));
          dispatch.stagedFormKind = ObjectArxBridge::DocContextDispatch::
              StagedFormKind::EvalValueEmitter;
          dispatch.bridgeSymbolsMayBeRetained = true;
          evaluateStagedForm = true;
          break;
        }

        case acadctl::NativeExecStepKind::Invalid:
        case acadctl::NativeExecStepKind::Done:
          break;
        }

        if (evaluateStagedForm) {
          break;
        }

        if (!acadctl::complete_execution_step(dispatch.jobId,
                                              std::move(stepResult))) {
          dispatch.dispatchResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
        }
      }
    }
  } catch (...) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecBridgeFailed, RTERROR);
  }

  const int returnStatus = evaluateStagedForm ? acedRetT() : acedRetNil();

  if (returnStatus != RTNORM) {
    dispatch.dispatchResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecBridgeFailed, returnStatus);
    evaluateStagedForm = false;
  }

  if (!evaluateStagedForm) {
    finishOutputPort(dispatch.outputPort, false);
  }

  const ObjectArxBridge::AdvanceCompletion completion =
      evaluateStagedForm
          ? ObjectArxBridge::AdvanceCompletion::EvaluateStagedForm
      : returnStatus == RTNORM ? ObjectArxBridge::AdvanceCompletion::ExitReady
                               : ObjectArxBridge::AdvanceCompletion::ExitFailed;
  bridge->finishAdvanceCallback(completion);

  return returnStatus == RTNORM ? RSRSLT : RSERR;
}

void ObjectArxBridge::runQueuedHistoryCommand() {
  ObjectArxBridge* bridge = commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      bridge->documentContextDispatch_->phase !=
          DocContextDispatch::Phase::Queued) {
    return;
  }

  DocContextDispatch& dispatch = *bridge->documentContextDispatch_;

  if (dispatch.kind == DocContextDispatch::Kind::ExecDriver) {
    return;
  }

  dispatch.phase = DocContextDispatch::Phase::Running;
  AcApDocument* target = bridge->document(dispatch.documentToken);

  if (!target) {
    dispatch.dispatchResult =
        result(acadctl::NativeActionResultKind::DrawingGone);
  } else if (!matchesDatabase(target, dispatch.databaseToken)) {
    dispatch.dispatchResult =
        result(acadctl::NativeActionResultKind::DrawingGenerationChanged);
  } else if (acDocManager->mdiActiveDocument() != target ||
             acDocManager->curDocument() != target) {
    dispatch.dispatchResult =
        nativeFailure(acadctl::NativeActionResultKind::DocumentContextFailed,
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
          dispatch.kind == DocContextDispatch::Kind::Redo ? ACRX_T("_.REDO")
                                                          : ACRX_T("_.U"),
          RTNONE);

      if (acDocManager->mdiActiveDocument() != target ||
          acDocManager->curDocument() != target) {
        dispatch.dispatchResult = nativeFailure(
            acadctl::NativeActionResultKind::DocumentContextFailed,
            Acad::eInvalidContext);
      } else if (!matchesDatabase(target, dispatch.databaseToken)) {
        dispatch.dispatchResult =
            result(acadctl::NativeActionResultKind::DrawingGenerationChanged);
      } else if (status != RTNORM) {
        dispatch.dispatchResult = bridgeFailure(
            acadctl::NativeActionResultKind::HistoryFailed, status);
      }
    }
  }

  bridge->scheduleDocumentContextFinalizer();
}

acadctl::NativeExecFinalizationObservation
ObjectArxBridge::finalizationObservation(const DocContextDispatch& dispatch) {
  return {
      dispatch.undoGroup != UndoGroupState::Inactive,
      dispatch.bridgeSymbolsMayBeRetained,
      dispatch.stagedFormKind != DocContextDispatch::StagedFormKind::None,
      dispatch.outputPort.has_value(),
      dispatch.terminalCleanupFailed,
  };
}

void ObjectArxBridge::finalizeDocumentContextDispatch(void*) {
  NativeActionCallbackLease callbackLease;
  ObjectArxBridge* bridge = commandBridge_;

  if (!bridge || !bridge->documentContextDispatch_ ||
      bridge->documentContextDispatch_->phase !=
          DocContextDispatch::Phase::Finalizing) {
    return;
  }

  DocContextDispatch& dispatch = *bridge->documentContextDispatch_;

  if (dispatch.restorePreviousActive) {
    AcApDocument* previousActive =
        bridge->document(dispatch.previousActiveToken);
    Acad::ErrorStatus restoreStatus = Acad::eNoDocument;

    if (matchesDatabase(previousActive, dispatch.previousActiveDatabaseToken)) {
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
  acadctl::NativeActionResult dispatchResult =
      std::move(dispatch.dispatchResult);
  const bool executionDriver =
      dispatch.kind == DocContextDispatch::Kind::ExecDriver;
  const acadctl::NativeExecFinalizationObservation observation =
      finalizationObservation(dispatch);
  bridge->documentContextDispatch_.reset();
  if (executionDriver) {
    acadctl::complete_execution_native_action(jobId, std::move(dispatchResult),
                                              observation);
  } else {
    acadctl::complete_native_action(jobId, std::move(dispatchResult));
  }
  scheduleNextNativeAction();
}

void ObjectArxBridge::publishDocumentSnapshot() {
  rust::Vec<acadctl::NativeDocumentSnapshot> states;

  for (DocSubscription& subscription : subscriptions_) {
    refreshSubscription(subscription);

    if (!subscription.database) {
      continue;
    }

    AcApDocument* document = subscription.document;
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
        acDocManager->mdiActiveDocument() == document,
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
  for (DocSubscription& subscription : subscriptions_) {
    drainDatabaseChanges(subscription);
  }
}

void ObjectArxBridge::drainDatabaseChanges(DocSubscription& subscription) {
  if (subscription.databaseReactor &&
      subscription.databaseReactor->takeChanged()) {
    documentSnapshotStale_.store(true, std::memory_order_relaxed);
  }
}

void ObjectArxBridge::eraseDatabaseReactor(DatabaseReactor* reactor) {
  const auto owned = std::find_if(
      databaseReactors_.begin(), databaseReactors_.end(),
      [reactor](const auto& candidate) { return candidate.get() == reactor; });

  if (owned != databaseReactors_.end()) {
    databaseReactors_.erase(owned);
  }
}

void ObjectArxBridge::detachDatabaseReactor(DocSubscription& subscription) {
  if (!subscription.databaseReactor) {
    return;
  }

  if (subscription.databaseReactor->databaseGone()) {
    drainDatabaseChanges(subscription);
    DatabaseReactor* reactor = subscription.databaseReactor;
    subscription.databaseReactor = nullptr;
    eraseDatabaseReactor(reactor);

    return;
  }

  const Acad::ErrorStatus status =
      subscription.database
          ? subscription.database->removeReactor(subscription.databaseReactor)
          : Acad::eNullPtr;
  drainDatabaseChanges(subscription);

  if (status == Acad::eOk || status == Acad::eKeyNotFound) {
    DatabaseReactor* reactor = subscription.databaseReactor;
    subscription.databaseReactor = nullptr;
    eraseDatabaseReactor(reactor);

    return;
  }

  syslog(LOG_ERR, "acadctl could not detach a database observer: %d",
         static_cast<int>(status));
  databaseReactorOwnershipUncertain_ = true;
  subscription.databaseReactor = nullptr;
}

void ObjectArxBridge::refreshSubscription(DocSubscription& subscription) {
  if (subscription.databaseReactor &&
      subscription.databaseReactor->databaseGone()) {
    AcDbDatabase* retiredDatabase = subscription.database;
    detachDatabaseReactor(subscription);
    subscription.database = nullptr;
    subscription.retiredDatabase = retiredDatabase;
    subscription.lispFunctionsDefined = false;
  }

  AcDbDatabase* database = subscription.document->database();

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
  DatabaseReactor* reactor = ownedReactor.get();
  databaseReactors_.push_back(std::move(ownedReactor));
  const Acad::ErrorStatus status = subscription.database->addReactor(reactor);

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

void ObjectArxBridge::subscribe(AcApDocument* document) {
  const auto alreadySubscribed =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocSubscription& subscription) {
                     return subscription.document == document;
                   });

  if (alreadySubscribed != subscriptions_.end()) {
    return;
  }

  subscriptions_.push_back(
      DocSubscription{document, nullptr, nullptr, false, nullptr});
  refreshSubscription(subscriptions_.back());
}

void ObjectArxBridge::databaseWillBeDestroyed(AcDbDatabase* database) {
  bool retiredSubscription = false;

  for (DocSubscription& subscription : subscriptions_) {
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

void ObjectArxBridge::actionTargetWillBeDestroyed(AcApDocument* document) {
  if (!documentContextDispatch_ ||
      documentContextDispatch_->phase != DocContextDispatch::Phase::Queued) {
    return;
  }

  const std::size_t documentToken =
      static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(document));

  if (documentContextDispatch_->documentToken != documentToken) {
    return;
  }

  documentContextDispatch_->dispatchResult =
      result(acadctl::NativeActionResultKind::DrawingGone);
  scheduleDocumentContextFinalizer();
}

void ObjectArxBridge::unsubscribe(AcApDocument* document) {
  const auto subscription =
      std::find_if(subscriptions_.begin(), subscriptions_.end(),
                   [document](const DocSubscription& candidate) {
                     return candidate.document == document;
                   });

  if (subscription == subscriptions_.end()) {
    return;
  }

  detachDatabaseReactor(*subscription);
  subscriptions_.erase(subscription);
}

std::unique_ptr<ObjectArxBridge> objectArxBridge;

void processNextAction(void*) {
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

void acadctl_disable_native_wakes() {
  acceptNativeActionWakes.store(false, std::memory_order_seq_cst);
}

std::uint32_t acadctl_native_callbacks_outstanding() {
  return nativeActionCallbacksOutstanding.load(std::memory_order_seq_cst);
}

void acadctl_create_bridge() {
  objectArxBridge = std::make_unique<ObjectArxBridge>();
}

Acad::ErrorStatus acadctl_start_bridge() { return objectArxBridge->start(); }

bool acadctl_stop_bridge() {
  return !objectArxBridge || objectArxBridge->stop();
}

void acadctl_destroy_bridge() { objectArxBridge.reset(); }

int acadctl_load_doc() {
  const int status = defineLispFunctions();

  if (objectArxBridge) {
    objectArxBridge->setLispFunctionsDefined(acDocManager->curDocument(),
                                             status == RTNORM);
  }

  return status;
}

int acadctl_unload_doc() {
  AcApDocument* document = acDocManager->curDocument();
  const int status = undefineLispFunctions();

  if (objectArxBridge) {
    objectArxBridge->setLispFunctionsDefined(document, false);
  }

  return status;
}
