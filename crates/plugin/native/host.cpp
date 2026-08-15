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
#include <string>
#include <syslog.h>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);
int acdbSetDbmod(AcDbDatabase *database, int value);
extern "C" int acadctl_wake_native_actions();

namespace {

std::atomic<std::uint32_t> nativeActionCallbacksOutstanding{0};
std::atomic<bool> acceptNativeActionWakes{true};
const ACHAR kDocumentActionCommandGroup[] = ACRX_T("ACADCTL_INTERNAL");
const ACHAR kDocumentActionCommandName[] = ACRX_T("ACADCTL_INTERNAL_ACTION");
const ACHAR kDocumentActionCommandInvocation[] =
    ACRX_T("ACADCTL_INTERNAL_ACTION\n");
const ACHAR kExecutionActionExpression[] =
    ACRX_T("(acadctl:_run-execution)");
const ACHAR kExecutionActionInvocation[] =
    ACRX_T("(acadctl:_run-execution)\n");

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
    acadctl::NativeActionResultKind kind, int status, const char *detail);

int acadctlExecuteAction() noexcept;

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

  bool beginDocumentAction(const acadctl::NativeAction &action,
                           acadctl::NativeActionResult &failure);

  void queueDocumentActionFinalizer();

  void queueExecutionActionFinalizer();

  void queuedDocumentActionTerminated(const ACHAR *commandName,
                                      bool cancelled);

  void queuedExecutionActionStarted(const ACHAR *firstLine);

  void queuedExecutionActionTerminated(bool cancelled);

  void failQueuedExecutionAction(bool cancelled);

  void finishExecutionActionCallback(bool evaluateStagedForm);

  struct PendingDocumentAction {
    enum class Phase { Queued, Running, Finalizing };
    enum class Kind { Undo, Redo, Execute };
    enum class Program { None, Form, EvalValue };

    std::uint64_t jobId;
    std::size_t documentToken;
    std::size_t databaseToken;
    std::size_t previousActiveToken;
    Kind kind;
    bool restorePreviousActive;
    acadctl::NativeActionResult commandResult;
    Phase phase;
    UndoGroupState undoGroup = UndoGroupState::Inactive;
    bool executionGroupStarted = false;
    bool formAttempted = false;
    Program program = Program::None;
    bool retainValue = false;
    bool reservedStateMayBeRetained = false;
    bool terminalReady = false;
    bool driverStarted = false;
    bool driverEnded = false;
    bool callbackActive = false;
    std::uint32_t lispDepth = 0;
    std::optional<rust::Box<acadctl::NativeValueWriter>> valueWriter;
  };

  static void executeDocumentAction();

  static void finishDocumentAction(void *data);

  friend int acadctlExecuteAction() noexcept;

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
      bridge_.queuedExecutionActionStarted(firstLine);
    }

    void commandEnded(const ACHAR *) override {
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandCancelled(const ACHAR *commandName) override {
      bridge_.queuedDocumentActionTerminated(commandName, true);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void commandFailed(const ACHAR *commandName) override {
      bridge_.queuedDocumentActionTerminated(commandName, false);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void lispEnded() override {
      bridge_.queuedExecutionActionTerminated(false);
      bridge_.refreshDocumentSnapshotIfStale();
      acadctl::native_state_may_be_ready();
    }

    void lispCancelled() override {
      bridge_.queuedExecutionActionTerminated(true);
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
  bool documentActionCommandRegistered_ = false;
  std::optional<PendingDocumentAction> pendingDocumentAction_;

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
    acadctl::NativeActionResultKind kind, int status, const char *detail) {
  return {kind, status, rust::String(detail)};
}

acadctl::NativeExecutionStepResult stepSuccess() {
  return {acadctl::NativeExecutionStepResultKind::Success, 0, 0,
          rust::String(), 0};
}

acadctl::NativeExecutionStepResult stepNativeFailure(int status,
                                                      const char *detail) {
  return {acadctl::NativeExecutionStepResultKind::NativeError, status, 0,
          rust::String(detail), 0};
}

struct ResbufDeleter {
  void operator()(resbuf *value) const {
    if (value) {
      acutRelRb(value);
    }
  }
};

using ResbufPtr = std::unique_ptr<resbuf, ResbufDeleter>;

constexpr int kPrintlnFunctionCode = 1;
constexpr int kEvalValueEventFunctionCode = 2;
constexpr int kExecuteActionFunctionCode = 3;
constexpr std::size_t kWideValueChunkUnits = 4096;

thread_local acadctl::NativeValueWriter *activeEvalValueWriter = nullptr;

std::size_t boundedWideChunkLength(const ACHAR *text);
rust::String boundedDiagnostic(const ACHAR *text, const char *fallback);
int integerValue(const resbuf *value);
bool matchesExecutionContext(AcApDocument *document,
                             std::size_t databaseToken,
                             AcApDocument *expectedActive);

acadctl::NativeValueEvent
valueEvent(acadctl::NativeValueEventKind kind) {
  return {kind};
}

bool writeValueEvent(acadctl::NativeValueWriter &writer,
                     acadctl::NativeValueEvent event,
                     rust::Str text = rust::Str()) {
  return acadctl::write_value_event(writer, event, text) ==
         acadctl::NativeValueWriteResult::Continue;
}

bool writeValueKind(acadctl::NativeValueWriter &writer,
                    acadctl::NativeValueEventKind kind) {
  return writeValueEvent(writer, valueEvent(kind));
}

bool writeTextEvent(acadctl::NativeValueWriter &writer,
                    acadctl::NativeValueEventKind kind,
                    const ACHAR *text) {
  if (!text) {
    return writeValueKind(writer, acadctl::NativeValueEventKind::Invalid);
  }
  for (const ACHAR *cursor = text; *cursor != 0;) {
    const std::size_t length = boundedWideChunkLength(cursor);
    const AcString chunk(cursor, static_cast<Adesk::UInt32>(length));
    const char *utf8 = chunk.utf8Ptr();
    if (!utf8 ||
        !writeValueEvent(writer, valueEvent(kind),
                         rust::Str(utf8, std::strlen(utf8)))) {
      return false;
    }
    cursor += length;
  }
  return true;
}

std::size_t boundedWideChunkLength(const ACHAR *text) {
  std::size_t length = 0;
  while (length < kWideValueChunkUnits && text[length] != 0) {
    ++length;
  }
  if constexpr (sizeof(ACHAR) == 2) {
    if (length == kWideValueChunkUnits && text[length] != 0) {
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

rust::String boundedDiagnostic(const ACHAR *text, const char *fallback) {
  if (!text) {
    return rust::String(fallback);
  }

  const std::size_t captureUnits = acadctl::native_diagnostic_capture_units();
  if (captureUnits < 2) {
    return rust::String(fallback);
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
    return rust::String(fallback);
  }
  std::string bounded(utf8);
  const std::size_t byteLimit = captureUnits - 1;
  if (truncated && bounded.size() <= byteLimit) {
    bounded.resize(byteLimit + 1, ' ');
  }
  return rust::String(bounded);
}

bool writeString(acadctl::NativeValueWriter &writer, const ACHAR *text) {
  if (!text) {
    return writeValueKind(writer, acadctl::NativeValueEventKind::Invalid);
  }
  if (!writeValueKind(writer, acadctl::NativeValueEventKind::BeginString)) {
    return false;
  }

  if (!writeTextEvent(writer,
                      acadctl::NativeValueEventKind::StringChunk, text)) {
    return false;
  }
  return writeValueKind(writer, acadctl::NativeValueEventKind::EndString);
}

bool writeEntity(acadctl::NativeValueWriter &writer, const ads_name name) {
  acadctl::NativeValueEvent event =
      valueEvent(acadctl::NativeValueEventKind::Entity);
  AcDbObjectId objectId;
  if (acdbGetObjectId(objectId, name) != Acad::eOk || objectId.isNull()) {
    return writeValueEvent(writer, event);
  }

  ACHAR handleText[AcDbHandle::kStrSiz]{};
  if (!objectId.handle().getIntoAsciiBuffer(handleText)) {
    return writeValueEvent(writer, event);
  }
  const AcString utf8Handle(handleText);
  event.has_payload = true;
  return writeValueEvent(writer, event, rust::Str(utf8Handle.utf8Ptr()));
}

bool writeResbufNode(acadctl::NativeValueWriter &writer,
                     const resbuf &node) {
  switch (node.restype) {
  case RTLB:
    return writeValueKind(writer, acadctl::NativeValueEventKind::BeginList);
  case RTLE:
    return writeValueKind(writer, acadctl::NativeValueEventKind::EndList);
  case RTDOTE:
    return writeValueKind(writer, acadctl::NativeValueEventKind::Dot);
  case RTNIL:
    return writeValueKind(writer, acadctl::NativeValueEventKind::Nil);
  case RTT:
    return writeValueKind(writer, acadctl::NativeValueEventKind::True);
  case RTVOID:
    return writeValueKind(writer, acadctl::NativeValueEventKind::Void);
  case RTSHORT: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Integer);
    event.integer = node.resval.rint;
    return writeValueEvent(writer, event);
  }
  case RTLONG: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Integer);
    event.integer = node.resval.rlong;
    return writeValueEvent(writer, event);
  }
  case RTINT64: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Integer);
    event.integer = node.resval.mnInt64;
    return writeValueEvent(writer, event);
  }
  case RTREAL:
  case RTANG:
  case RTORINT: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Real);
    event.real = node.resval.rreal;
    return writeValueEvent(writer, event);
  }
  case RTPOINT: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Point2);
    event.x = node.resval.rpoint[0];
    event.y = node.resval.rpoint[1];
    return writeValueEvent(writer, event);
  }
  case RT3DPOINT: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Point3);
    event.x = node.resval.rpoint[0];
    event.y = node.resval.rpoint[1];
    event.z = node.resval.rpoint[2];
    return writeValueEvent(writer, event);
  }
  case RTSTR:
    return writeString(writer, node.resval.rstring);
  case RTENAME:
    return writeEntity(writer, node.resval.rlname);
  case RTPICKS:
    return writeValueKind(writer,
                          acadctl::NativeValueEventKind::SelectionSet);
  default: {
    acadctl::NativeValueEvent event =
        valueEvent(acadctl::NativeValueEventKind::Unsupported);
    event.native_type = static_cast<std::uint32_t>(
        static_cast<std::int32_t>(node.restype));
    event.has_payload = true;
    return writeValueEvent(writer, event);
  }
  }
}

void writeResbufSequence(acadctl::NativeValueWriter &writer,
                         const resbuf *head) {
  const resbuf *slow = head;
  const resbuf *fast = head;
  for (const resbuf *node = head; node; node = node->rbnext) {
    if (!writeResbufNode(writer, *node)) {
      return;
    }

    slow = slow ? slow->rbnext : nullptr;
    fast = fast && fast->rbnext ? fast->rbnext->rbnext : nullptr;
    if (slow && slow == fast) {
      writeValueKind(writer, acadctl::NativeValueEventKind::Invalid);
      return;
    }
  }
}

int acadctlPrintln() noexcept {
  try {
    AcApDocument *document = acDocManager->curDocument();
    AcDbDatabase *database = document ? document->database() : nullptr;
    rust::Box<acadctl::NativeValueWriter> writer = acadctl::begin_println(
        static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(document)),
        static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(database)));
    if (acadctl::value_writer_active(*writer)) {
      writeResbufSequence(*writer, acedGetArgs());
    }
    const int returnStatus = acedRetNil();
    if (returnStatus != RTNORM) {
      writeValueKind(*writer, acadctl::NativeValueEventKind::Invalid);
    }
    acadctl::finish_value_writer(std::move(writer));
    return returnStatus == RTNORM ? RSRSLT : RSERR;
  } catch (...) {
    syslog(LOG_ERR, "acadctl:println bridge failed unexpectedly");
    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }
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
    if (activeEvalValueWriter) {
      keepGoing =
          writePrivateValueEvent(*activeEvalValueWriter, acedGetArgs());
    }
    const int returnStatus = keepGoing ? acedRetT() : acedRetNil();
    if (returnStatus != RTNORM && activeEvalValueWriter) {
      writeValueKind(*activeEvalValueWriter,
                     acadctl::NativeValueEventKind::Invalid);
    }
    return returnStatus == RTNORM ? RSRSLT : RSERR;
  } catch (...) {
    if (activeEvalValueWriter) {
      writeValueKind(*activeEvalValueWriter,
                     acadctl::NativeValueEventKind::Invalid);
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
  int status = defineLispFunction(ACRX_T("acadctl:println"),
                                  kPrintlnFunctionCode,
                                  &acadctlPrintln);
  if (status == RTNORM) {
    status = defineLispFunction(ACRX_T("acadctl:_value-event"),
                                kEvalValueEventFunctionCode,
                                &acadctlEvalValueEvent);
  }
  if (status == RTNORM) {
    status = defineLispFunction(ACRX_T("acadctl:_execute-action"),
                                kExecuteActionFunctionCode,
                                &acadctlExecuteAction);
  }
  if (status != RTNORM) {
    acedUndef(ACRX_T("acadctl:_value-event"),
              kEvalValueEventFunctionCode);
    acedUndef(ACRX_T("acadctl:println"), kPrintlnFunctionCode);
  }
  return status;
}

int undefineLispFunctions() {
  const int executionStatus =
      acedUndef(ACRX_T("acadctl:_execute-action"),
                kExecuteActionFunctionCode);
  const int privateStatus =
      acedUndef(ACRX_T("acadctl:_value-event"),
                kEvalValueEventFunctionCode);
  const int publicStatus =
      acedUndef(ACRX_T("acadctl:println"), kPrintlnFunctionCode);
  if (executionStatus != RTNORM) {
    return executionStatus;
  }
  return privateStatus != RTNORM ? privateStatus : publicStatus;
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

int clearEvaluationSymbols(bool includeValue = true) {
  int firstFailure = RTNORM;
  for (const ACHAR *name : {ACRX_T("acadctl:*source*"),
                            ACRX_T("acadctl:*program*"),
                            ACRX_T("acadctl:*status*"),
                            ACRX_T("acadctl:*error*"),
                            ACRX_T("acadctl:*errno*")}) {
    const int status = clearSymbol(name);
    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }
  if (includeValue) {
    const int status = clearSymbol(ACRX_T("acadctl:*value*"));
    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }
  return firstFailure;
}

struct ReservedStateStepResult {
  acadctl::NativeExecutionStepResult result;
  bool reservedStateRetained;
};

ReservedStateStepResult finishEvaluation(
    acadctl::NativeExecutionStepResult result, bool retainValue) {
  const bool successful =
      result.kind == acadctl::NativeExecutionStepResultKind::Success;
  const int cleanupStatus =
      clearEvaluationSymbols(!(successful && retainValue));
  if (cleanupStatus == RTNORM) {
    return {std::move(result), successful && retainValue};
  }
  const bool reservedStateStillRetained =
      clearEvaluationSymbols() != RTNORM;
  result.evaluator_state_cleanup_status = cleanupStatus;
  return {std::move(result), reservedStateStillRetained};
}

ReservedStateStepResult stageEvaluation(rust::Str source,
                                        const AcString &evaluatorText,
                                        bool retainValue) {
  const AcString pending(ACRX_T("pending"));
  const int clearStatus = clearEvaluationSymbols();
  if (clearStatus != RTNORM) {
    return {stepNativeFailure(
                clearStatus,
                "could not clear the reserved AutoLISP evaluator state"),
            clearEvaluationSymbols() != RTNORM};
  }
  {
    const AcString form(source.data(), AcString::Utf8,
                        static_cast<Adesk::UInt32>(source.size()));
    if (putStringSymbol(ACRX_T("acadctl:*source*"), form) != RTNORM ||
        putStringSymbol(ACRX_T("acadctl:*program*"), evaluatorText) !=
            RTNORM ||
        putStringSymbol(ACRX_T("acadctl:*status*"), pending) != RTNORM) {
      return finishEvaluation(
          stepNativeFailure(RTERROR,
                            "could not stage the AutoLISP form in memory"),
          retainValue);
    }
  }
  return {stepSuccess(), true};
}

ReservedStateStepResult collectEvaluation(bool retainValue) {
  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(ACRX_T("acadctl:*status*"), statusResult);
  const bool nilStatus =
      statusResult == RTNIL ||
      (statusResult == RTNORM && !status) ||
      (statusResult == RTNORM && status && status->restype == RTNIL);
  if (statusResult != RTNORM && !nilStatus) {
    return finishEvaluation(
        stepNativeFailure(statusResult,
                          "the evaluator did not publish a result"),
        retainValue);
  }

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(ACRX_T("acadctl:*errno*"), errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;

  if (!nilStatus && status && status->restype == RTT) {
    return finishEvaluation(stepSuccess(), retainValue);
  }
  if (!nilStatus) {
    return finishEvaluation(
        stepNativeFailure(RTERROR,
                          "the evaluator published an invalid result tag"),
        retainValue);
  }

  int errorResult = RTERROR;
  ResbufPtr error = getSymbol(ACRX_T("acadctl:*error*"), errorResult);
  rust::String detail("AutoLISP evaluation failed");
  if (errorResult == RTNORM && error && error->restype == RTSTR &&
      error->resval.rstring) {
    detail = boundedDiagnostic(error->resval.rstring,
                               "AutoLISP evaluation failed");
  }
  return finishEvaluation(
      {acadctl::NativeExecutionStepResultKind::LispError, 0, lispErrnoValue,
       std::move(detail), 0},
      retainValue);
}

acadctl::NativeExecutionStepResult valueVisitorOutcome(int commandStatus) {
  if (commandStatus != RTNORM) {
    return stepNativeFailure(commandStatus,
                             "AutoCAD rejected the eval value visitor");
  }

  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(ACRX_T("acadctl:*status*"), statusResult);
  const bool nilStatus =
      statusResult == RTNIL ||
      (statusResult == RTNORM && !status) ||
      (statusResult == RTNORM && status && status->restype == RTNIL);
  if (statusResult != RTNORM && !nilStatus) {
    return stepNativeFailure(statusResult,
                             "the eval value visitor did not publish a result");
  }
  if (!nilStatus && status && status->restype == RTT) {
    return stepSuccess();
  }
  if (!nilStatus) {
    return stepNativeFailure(
        RTERROR, "the eval value visitor published an invalid result tag");
  }

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(ACRX_T("acadctl:*errno*"), errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;
  int errorResult = RTERROR;
  ResbufPtr error = getSymbol(ACRX_T("acadctl:*error*"), errorResult);
  rust::String detail("AutoLISP eval value traversal failed");
  if (errorResult == RTNORM && error && error->restype == RTSTR &&
      error->resval.rstring) {
    detail = boundedDiagnostic(error->resval.rstring,
                               "AutoLISP eval value traversal failed");
  }
  return {acadctl::NativeExecutionStepResultKind::LispError, 0,
          lispErrnoValue, std::move(detail), 0};
}

ReservedStateStepResult finishEvalValueEmission(
    acadctl::NativeExecutionStepResult result) {
  const int cleanupStatus = clearEvaluationSymbols();
  if (cleanupStatus == RTNORM) {
    return {std::move(result), false};
  }
  const bool reservedStateStillRetained =
      clearEvaluationSymbols() != RTNORM;
  result.evaluator_state_cleanup_status = cleanupStatus;
  return {std::move(result), reservedStateStillRetained};
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
    return {stepNativeFailure(commandStatus, "the UNDO command failed"),
            state};
  }
  if (state == UndoGroupState::Unknown) {
    return {stepNativeFailure(observationStatus,
                              "could not read AutoCAD's undo state"),
            state};
  }
  if (state != expectedState) {
    return {stepNativeFailure(
                RTERROR,
                expectedState == UndoGroupState::Active
                    ? "AutoCAD did not open the requested undo group"
                    : "AutoCAD did not close the requested undo group"),
            state};
  }
  return {stepSuccess(), state};
}

UndoCommandResult rollbackUndoGroup(UndoGroupState state,
                                    bool executionGroupStarted) {
  if (!executionGroupStarted || state == UndoGroupState::Unknown) {
    return {stepNativeFailure(
                RTERROR,
                "the execution undo group could not be identified for rollback"),
            state};
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
    return {stepNativeFailure(status, "the U command failed"), finalState};
  }
  if (finalState != UndoGroupState::Inactive) {
    return {stepNativeFailure(
                observationStatus,
                "could not prove that rollback closed the execution undo group"),
            finalState};
  }
  if (end.result.kind !=
      acadctl::NativeExecutionStepResultKind::Success) {
    return {std::move(end.result), finalState};
  }
  return {stepSuccess(), finalState};
}

bool matchesDatabase(AcApDocument *document, std::size_t databaseToken) {
  return static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
             document->database())) == databaseToken;
}

bool matchesExecutionContext(AcApDocument *document,
                             std::size_t databaseToken,
                             AcApDocument *expectedActive) {
  return matchesDatabase(document, databaseToken) &&
         acDocManager->curDocument() == document &&
         acDocManager->mdiActiveDocument() == expectedActive;
}

int clearReservedStateIfSafe(AcApDocument *document,
                             std::size_t databaseToken,
                             AcApDocument *expectedActive,
                             bool &reservedStateMayBeRetained) {
  if (!reservedStateMayBeRetained) {
    return RTNORM;
  }
  if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
    return RTREJ;
  }
  const int cleanupStatus = clearEvaluationSymbols();
  if (cleanupStatus == RTNORM) {
    reservedStateMayBeRetained = false;
  }
  return cleanupStatus;
}

acadctl::NativeActionResult abandonLostExecutionContext(
    std::uint64_t jobId, AcApDocument *document,
    std::size_t databaseToken, AcApDocument *expectedActive,
    bool undoGroupMayBeOpen,
    bool &reservedStateMayBeRetained) {
  const int cleanupStatus = clearReservedStateIfSafe(
      document, databaseToken, expectedActive, reservedStateMayBeRetained);
  const char *detail =
      cleanupStatus == RTNORM
          ? "the target document context changed during execution"
          : cleanupStatus == RTREJ
                ? "the target document context changed and its retained AutoLISP value could not be cleared safely"
                : "the target document context changed and retained-value cleanup failed";
  const bool quarantine =
      undoGroupMayBeOpen || reservedStateMayBeRetained;
  if (!acadctl::abandon_execution(
          jobId,
          stepNativeFailure(cleanupStatus == RTNORM ? RTERROR : cleanupStatus,
                            detail))) {
    return bridgeFailure(
        quarantine
            ? acadctl::NativeActionResultKind::ExecutionCleanupFailed
            : acadctl::NativeActionResultKind::ExecutionBridgeFailed,
        RTERROR,
        "Rust could not terminalize an execution after context loss");
  }
  return quarantine
             ? bridgeFailure(
                   acadctl::NativeActionResultKind::ExecutionCleanupFailed,
                   RTERROR,
                   "context loss left native execution state unproved")
             : result(acadctl::NativeActionResultKind::Success);
}

void scheduleNextNativeAction() {
  if (!acadctl::native_actions_need_wake()) {
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
  const Acad::ErrorStatus commandStatus = acedRegCmds->addCommand(
      kDocumentActionCommandGroup, kDocumentActionCommandName,
      kDocumentActionCommandName,
      ACRX_CMD_MODAL | ACRX_CMD_NOHISTORY | ACRX_CMD_NO_UNDO_MARKER,
      executeDocumentAction);
  if (commandStatus != Acad::eOk) {
    return commandStatus;
  }
  documentActionCommandRegistered_ = true;

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
  if (pendingDocumentAction_) {
    return false;
  }
  if (documentActionCommandRegistered_) {
    const Acad::ErrorStatus status =
        acedRegCmds->removeGroup(kDocumentActionCommandGroup);
    if (status != Acad::eOk && status != Acad::eKeyNotFound) {
      return false;
    }
    documentActionCommandRegistered_ = false;
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
  case acadctl::NativeActionKind::RunExecution:
    if (beginDocumentAction(action, actionResult)) {
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
  if (subscription != subscriptions_.end()) {
    if (defined) {
      refreshSubscription(*subscription);
    }
    subscription->lispFunctionsDefined = defined;
  }
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

bool ObjectArxBridge::beginDocumentAction(
    const acadctl::NativeAction &action,
    acadctl::NativeActionResult &failure) {
  if (pendingDocumentAction_) {
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::ContextFailed, RTERROR,
        "a native document action is already pending");
    return false;
  }

  PendingDocumentAction::Kind kind;
  switch (action.kind) {
  case acadctl::NativeActionKind::Undo:
    kind = PendingDocumentAction::Kind::Undo;
    break;
  case acadctl::NativeActionKind::Redo:
    kind = PendingDocumentAction::Kind::Redo;
    break;
  case acadctl::NativeActionKind::RunExecution:
    kind = PendingDocumentAction::Kind::Execute;
    break;
  default:
    failure = bridgeFailure(
        acadctl::NativeActionResultKind::ContextFailed, RTERROR,
        "Rust requested an unsupported native document action");
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
  if (kind == PendingDocumentAction::Kind::Execute) {
    if (!lispFunctionsDefined(target)) {
      failure = bridgeFailure(
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
          "acadctl AutoLISP functions are unavailable in the target drawing");
      return false;
    }
    if (!target->database()->undoRecording()) {
      failure = result(acadctl::NativeActionResultKind::UndoDisabled);
      return false;
    }
  }
  AcApDocument *previousActive = acDocManager->mdiActiveDocument();
  if (!previousActive) {
    failure = nativeFailure(acadctl::NativeActionResultKind::ContextFailed,
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
    failure = bridgeFailure(acadctl::NativeActionResultKind::ContextFailed,
                            RTERROR,
                            "could not read AutoCAD's pending command input");
    return false;
  }

  pendingDocumentAction_.emplace(PendingDocumentAction{
      action.job_id,
      action.document_token,
      action.database_token,
      static_cast<std::size_t>(
          reinterpret_cast<std::uintptr_t>(previousActive)),
      kind,
      restorePreviousActive,
      result(acadctl::NativeActionResultKind::Success),
      PendingDocumentAction::Phase::Queued,
  });
  nativeActionCallbacksOutstanding.fetch_add(1, std::memory_order_seq_cst);
  const ACHAR *invocation =
      kind == PendingDocumentAction::Kind::Execute
          ? kExecutionActionInvocation
          : kDocumentActionCommandInvocation;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->sendStringToExecute(target, invocation, true, false,
                                        false);
  if (scheduleStatus == Acad::eOk) {
    return true;
  }
  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
  pendingDocumentAction_.reset();
  failure = nativeFailure(acadctl::NativeActionResultKind::ContextFailed,
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
        acadctl::NativeActionResultKind::ContextCleanupFailed,
        restoreStatus == Acad::eOk ? Acad::eInvalidContext : restoreStatus);
  }
  return false;
}

void ObjectArxBridge::queueDocumentActionFinalizer() {
  if (!pendingDocumentAction_) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  pending.phase = PendingDocumentAction::Phase::Finalizing;
  const Acad::ErrorStatus scheduleStatus =
      acDocManager->beginExecuteInApplicationContext(finishDocumentAction,
                                                     nullptr);
  if (scheduleStatus == Acad::eOk) {
    return;
  }

  pending.commandResult = nativeFailure(
      acadctl::NativeActionResultKind::ContextCleanupFailed, scheduleStatus);
  const std::uint64_t jobId = pending.jobId;
  acadctl::NativeActionResult commandResult = std::move(pending.commandResult);
  pendingDocumentAction_.reset();
  acadctl::complete_native_action(jobId, std::move(commandResult));
  nativeActionCallbacksOutstanding.fetch_sub(1, std::memory_order_seq_cst);
}

void ObjectArxBridge::queuedDocumentActionTerminated(
    const ACHAR *commandName, bool cancelled) {
  if (!pendingDocumentAction_ ||
      pendingDocumentAction_->phase != PendingDocumentAction::Phase::Queued ||
      !commandName) {
    return;
  }
  if (pendingDocumentAction_->kind ==
      PendingDocumentAction::Kind::Execute) {
    return;
  }
  const std::size_t commandLength =
      std::char_traits<ACHAR>::length(commandName);
  const std::size_t expectedLength =
      std::char_traits<ACHAR>::length(kDocumentActionCommandName);
  if (commandLength != expectedLength ||
      std::char_traits<ACHAR>::compare(commandName,
                                      kDocumentActionCommandName,
                                      expectedLength) != 0) {
    return;
  }

  const bool execution = pendingDocumentAction_->kind ==
                         PendingDocumentAction::Kind::Execute;
  pendingDocumentAction_->commandResult = bridgeFailure(
      execution ? acadctl::NativeActionResultKind::ExecutionBridgeFailed
                : acadctl::NativeActionResultKind::HistoryFailed,
      RTERROR,
      cancelled ? "AutoCAD cancelled the queued document action"
                : "AutoCAD failed the queued document action");
  queueDocumentActionFinalizer();
}

void ObjectArxBridge::queuedExecutionActionStarted(const ACHAR *firstLine) {
  if (!pendingDocumentAction_ ||
      (pendingDocumentAction_->phase !=
           PendingDocumentAction::Phase::Queued &&
       pendingDocumentAction_->phase !=
           PendingDocumentAction::Phase::Running) ||
      pendingDocumentAction_->kind != PendingDocumentAction::Kind::Execute ||
      !firstLine) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  if (!pending.driverStarted) {
    const std::size_t actualLength =
        std::char_traits<ACHAR>::length(firstLine);
    const std::size_t expectedLength =
        std::char_traits<ACHAR>::length(kExecutionActionExpression);
    if (actualLength != expectedLength ||
        std::char_traits<ACHAR>::compare(
            firstLine, kExecutionActionExpression, expectedLength) != 0) {
      return;
    }
    pending.driverStarted = true;
    pending.lispDepth = 1;
    return;
  }

  if (pending.lispDepth == std::numeric_limits<std::uint32_t>::max()) {
    failQueuedExecutionAction(false);
    return;
  }
  ++pending.lispDepth;
}

void ObjectArxBridge::failQueuedExecutionAction(bool cancelled) {
  if (!pendingDocumentAction_ ||
      pendingDocumentAction_->kind != PendingDocumentAction::Kind::Execute ||
      pendingDocumentAction_->phase ==
          PendingDocumentAction::Phase::Finalizing) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  if (pending.valueWriter) {
    activeEvalValueWriter = nullptr;
    rust::Box<acadctl::NativeValueWriter> writer =
        std::move(*pending.valueWriter);
    pending.valueWriter.reset();
    acadctl::finish_value_writer(std::move(writer));
  }

  pending.commandResult = bridgeFailure(
      acadctl::NativeActionResultKind::ExecutionBridgeFailed,
      RTERROR,
      cancelled ? "AutoCAD cancelled the queued execution action"
                : "AutoCAD terminated the queued execution action prematurely");
  queueExecutionActionFinalizer();
}

void ObjectArxBridge::queueExecutionActionFinalizer() {
  if (!pendingDocumentAction_ ||
      pendingDocumentAction_->kind != PendingDocumentAction::Kind::Execute) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  const bool cleanupUnproved =
      pending.undoGroup != UndoGroupState::Inactive ||
      pending.reservedStateMayBeRetained ||
      pending.program != PendingDocumentAction::Program::None ||
      pending.valueWriter.has_value();
  if (cleanupUnproved &&
      pending.commandResult.kind !=
          acadctl::NativeActionResultKind::ContextCleanupFailed) {
    pending.commandResult.kind =
        acadctl::NativeActionResultKind::ExecutionCleanupFailed;
  }
  queueDocumentActionFinalizer();
}

void ObjectArxBridge::queuedExecutionActionTerminated(bool cancelled) {
  if (!pendingDocumentAction_ ||
      (pendingDocumentAction_->phase !=
           PendingDocumentAction::Phase::Queued &&
       pendingDocumentAction_->phase !=
           PendingDocumentAction::Phase::Running) ||
      pendingDocumentAction_->kind != PendingDocumentAction::Kind::Execute ||
      !pendingDocumentAction_->driverStarted) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  if (cancelled) {
    failQueuedExecutionAction(true);
    return;
  }
  if (pending.lispDepth > 1) {
    --pending.lispDepth;
    return;
  }
  pending.lispDepth = 0;
  pending.driverEnded = true;
  if (pending.terminalReady) {
    queueExecutionActionFinalizer();
  } else if (!pending.callbackActive) {
    failQueuedExecutionAction(false);
  }
}

void ObjectArxBridge::finishExecutionActionCallback(
    bool evaluateStagedForm) {
  if (!pendingDocumentAction_ ||
      pendingDocumentAction_->kind != PendingDocumentAction::Kind::Execute ||
      pendingDocumentAction_->phase ==
          PendingDocumentAction::Phase::Finalizing) {
    return;
  }

  PendingDocumentAction &pending = *pendingDocumentAction_;
  pending.callbackActive = false;
  if (!pending.driverEnded) {
    return;
  }
  if (!evaluateStagedForm && pending.terminalReady) {
    queueExecutionActionFinalizer();
  } else {
    failQueuedExecutionAction(false);
  }
}

int acadctlExecuteAction() noexcept {
  ObjectArxBridge *bridge = ObjectArxBridge::commandBridge_;
  if (!bridge || !bridge->pendingDocumentAction_ ||
      (bridge->pendingDocumentAction_->phase !=
           ObjectArxBridge::PendingDocumentAction::Phase::Queued &&
       bridge->pendingDocumentAction_->phase !=
           ObjectArxBridge::PendingDocumentAction::Phase::Running) ||
      bridge->pendingDocumentAction_->kind !=
          ObjectArxBridge::PendingDocumentAction::Kind::Execute) {
    return acedRetNil() == RTNORM ? RSRSLT : RSERR;
  }

  ObjectArxBridge::PendingDocumentAction &pending =
      *bridge->pendingDocumentAction_;
  if (!pending.driverStarted) {
    pending.driverStarted = true;
    pending.lispDepth = 1;
  }
  pending.phase = ObjectArxBridge::PendingDocumentAction::Phase::Running;
  pending.callbackActive = true;
  bool evaluateStagedForm = false;
  try {
    AcApDocument *target = bridge->document(pending.documentToken);
    if (!target) {
      pending.commandResult =
          result(acadctl::NativeActionResultKind::DocumentGone);
    } else if (!matchesDatabase(target, pending.databaseToken)) {
      pending.commandResult =
          result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
    } else if (acDocManager->mdiActiveDocument() != target ||
               acDocManager->curDocument() != target) {
      pending.commandResult = nativeFailure(
          acadctl::NativeActionResultKind::ContextFailed,
          Acad::eInvalidContext);
    } else if (!bridge->lispFunctionsDefined(target)) {
      pending.commandResult = bridgeFailure(
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
          "acadctl AutoLISP functions are unavailable in the target drawing");
    } else if (!target->database()->undoRecording()) {
      pending.commandResult =
          result(acadctl::NativeActionResultKind::UndoDisabled);
    } else {
      const rust::Str evaluator = acadctl::execution_evaluator_source();
      const AcString evaluatorText(
          evaluator.data(), AcString::Utf8,
          static_cast<Adesk::UInt32>(evaluator.size()));
      const rust::Str visitor = acadctl::execution_value_source();
      const AcString visitorText(
          visitor.data(), AcString::Utf8,
          static_cast<Adesk::UInt32>(visitor.size()));

      if (pending.program ==
          ObjectArxBridge::PendingDocumentAction::Program::Form) {
        ReservedStateStepResult evaluation = collectEvaluation(
            pending.retainValue);
        pending.reservedStateMayBeRetained = evaluation.reservedStateRetained;
        pending.program =
            ObjectArxBridge::PendingDocumentAction::Program::None;

        int observationStatus = RTERROR;
        pending.undoGroup = observeUndoGroup(observationStatus);
        if (evaluation.result.kind ==
                acadctl::NativeExecutionStepResultKind::Success &&
            pending.undoGroup != UndoGroupState::Active) {
          evaluation.result = stepNativeFailure(
              pending.undoGroup == UndoGroupState::Unknown
                  ? observationStatus
                  : RTERROR,
              "the execution undo group changed during AutoLISP evaluation");
        }
        if (!acadctl::complete_execution_step(
                pending.jobId, std::move(evaluation.result))) {
          pending.commandResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR, "Rust rejected an AutoLISP form result");
        }
      } else if (pending.program ==
                 ObjectArxBridge::PendingDocumentAction::Program::EvalValue) {
        activeEvalValueWriter = nullptr;
        ReservedStateStepResult emission = finishEvalValueEmission(
            valueVisitorOutcome(RTNORM));
        pending.reservedStateMayBeRetained =
            emission.reservedStateRetained;
        pending.program =
            ObjectArxBridge::PendingDocumentAction::Program::None;
        if (pending.valueWriter) {
          rust::Box<acadctl::NativeValueWriter> writer =
              std::move(*pending.valueWriter);
          pending.valueWriter.reset();
          acadctl::finish_value_writer(std::move(writer));
        }
        if (!acadctl::complete_execution_step(
                pending.jobId, std::move(emission.result))) {
          pending.commandResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR, "Rust rejected an eval value result");
        }
      }

      while (pending.commandResult.kind ==
             acadctl::NativeActionResultKind::Success) {
        if (!matchesExecutionContext(target, pending.databaseToken,
                                     target)) {
          pending.commandResult = abandonLostExecutionContext(
              pending.jobId, target, pending.databaseToken, target,
              pending.undoGroup != UndoGroupState::Inactive,
              pending.reservedStateMayBeRetained);
          break;
        }

        rust::Box<acadctl::NativeExecutionStep> step =
            acadctl::take_execution_step(pending.jobId);
        const acadctl::NativeExecutionStepKind kind =
            acadctl::execution_step_kind(*step);
        if (kind == acadctl::NativeExecutionStepKind::Done) {
          if (pending.undoGroup != UndoGroupState::Inactive) {
            UndoCommandResult cleanup =
                pending.formAttempted
                    ? rollbackUndoGroup(pending.undoGroup,
                                        pending.executionGroupStarted)
                    : runUndoCommand(ACRX_T("_End"),
                                     UndoGroupState::Inactive);
            pending.undoGroup = cleanup.state;
            if (cleanup.result.kind !=
                    acadctl::NativeExecutionStepResultKind::Success ||
                pending.undoGroup != UndoGroupState::Inactive) {
              pending.commandResult = bridgeFailure(
                  acadctl::NativeActionResultKind::ExecutionCleanupFailed,
                  RTERROR,
                  "the execution undo group could not be closed");
            }
          }
          if (pending.reservedStateMayBeRetained) {
            const int cleanupStatus = clearEvaluationSymbols();
            pending.reservedStateMayBeRetained = cleanupStatus != RTNORM;
            if (pending.reservedStateMayBeRetained) {
              pending.commandResult = bridgeFailure(
                  acadctl::NativeActionResultKind::EvaluatorStateCleanupFailed,
                  cleanupStatus,
                  "reserved AutoLISP evaluator state could not be cleared");
            }
          }
          break;
        }
        if (kind == acadctl::NativeExecutionStepKind::Invalid) {
          pending.commandResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR, "Rust returned an invalid execution step");
          break;
        }

        acadctl::NativeExecutionStepResult stepResult = stepSuccess();
        UndoCommandResult undoTransition{stepSuccess(), pending.undoGroup};
        switch (kind) {
        case acadctl::NativeExecutionStepKind::BeginUndoGroup:
          undoTransition =
              runUndoCommand(ACRX_T("_Begin"), UndoGroupState::Active);
          stepResult = std::move(undoTransition.result);
          pending.undoGroup = undoTransition.state;
          pending.executionGroupStarted =
              pending.undoGroup != UndoGroupState::Inactive;
          break;
        case acadctl::NativeExecutionStepKind::EvaluateForm: {
          pending.formAttempted = true;
          ReservedStateStepResult staging = stageEvaluation(
              acadctl::execution_step_source(*step), evaluatorText,
              acadctl::execution_step_retain_value(*step));
          pending.reservedStateMayBeRetained =
              staging.reservedStateRetained;
          if (staging.result.kind ==
              acadctl::NativeExecutionStepResultKind::Success) {
            pending.program =
                ObjectArxBridge::PendingDocumentAction::Program::Form;
            pending.retainValue =
                acadctl::execution_step_retain_value(*step);
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
          pending.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecutionStepKind::RollbackUndoGroup:
          undoTransition = rollbackUndoGroup(
              pending.undoGroup, pending.executionGroupStarted);
          stepResult = std::move(undoTransition.result);
          pending.undoGroup = undoTransition.state;
          break;
        case acadctl::NativeExecutionStepKind::ClearRetainedEvalValue: {
          const int cleanupStatus = clearEvaluationSymbols();
          stepResult = cleanupStatus == RTNORM
                           ? stepSuccess()
                           : stepNativeFailure(
                                 cleanupStatus,
                                 "could not clear the retained AutoLISP value");
          pending.reservedStateMayBeRetained = cleanupStatus != RTNORM;
          break;
        }
        case acadctl::NativeExecutionStepKind::EmitEvalValue: {
          rust::Box<acadctl::NativeValueWriter> writer =
              acadctl::begin_eval_value(
                  pending.jobId, pending.documentToken,
                  pending.databaseToken);
          if (!acadctl::value_writer_active(*writer)) {
            acadctl::finish_value_writer(std::move(writer));
            ReservedStateStepResult emission =
                finishEvalValueEmission(stepSuccess());
            stepResult = std::move(emission.result);
            pending.reservedStateMayBeRetained =
                emission.reservedStateRetained;
            break;
          }

          const AcString visitorPending(ACRX_T("pending"));
          int preparationStatus = clearEvaluationSymbols(false);
          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(
                ACRX_T("acadctl:*program*"), visitorText);
          }
          if (preparationStatus == RTNORM) {
            preparationStatus = putStringSymbol(
                ACRX_T("acadctl:*status*"), visitorPending);
          }
          if (preparationStatus != RTNORM || activeEvalValueWriter) {
            writeValueKind(*writer,
                           acadctl::NativeValueEventKind::Invalid);
            acadctl::finish_value_writer(std::move(writer));
            ReservedStateStepResult emission = finishEvalValueEmission(
                stepNativeFailure(
                    preparationStatus == RTNORM ? RTERROR
                                                : preparationStatus,
                    preparationStatus == RTNORM
                        ? "an eval value visitor was already active"
                        : "could not prepare the eval value visitor"));
            stepResult = std::move(emission.result);
            pending.reservedStateMayBeRetained =
                emission.reservedStateRetained;
            break;
          }

          pending.valueWriter.emplace(std::move(writer));
          activeEvalValueWriter = &**pending.valueWriter;
          pending.program =
              ObjectArxBridge::PendingDocumentAction::Program::EvalValue;
          pending.reservedStateMayBeRetained = true;
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
        if (!acadctl::complete_execution_step(pending.jobId,
                                               std::move(stepResult))) {
          pending.commandResult = bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionBridgeFailed,
              RTERROR, "Rust rejected a native execution step result");
        }
      }
    }
  } catch (...) {
    pending.commandResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
        "the native execution bridge threw an exception");
  }

  const int returnStatus = evaluateStagedForm ? acedRetT() : acedRetNil();
  if (returnStatus != RTNORM) {
    pending.commandResult = bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, returnStatus,
        "the execution bridge could not return to AutoLISP");
    evaluateStagedForm = false;
  }
  if (!evaluateStagedForm && pending.valueWriter) {
    activeEvalValueWriter = nullptr;
    rust::Box<acadctl::NativeValueWriter> writer =
        std::move(*pending.valueWriter);
    pending.valueWriter.reset();
    acadctl::finish_value_writer(std::move(writer));
  }
  if (!evaluateStagedForm && returnStatus == RTNORM) {
    pending.terminalReady = true;
  }
  bridge->finishExecutionActionCallback(evaluateStagedForm);
  return returnStatus == RTNORM ? RSRSLT : RSERR;
}

void ObjectArxBridge::executeDocumentAction() {
  ObjectArxBridge *bridge = commandBridge_;
  if (!bridge || !bridge->pendingDocumentAction_ ||
      bridge->pendingDocumentAction_->phase !=
          PendingDocumentAction::Phase::Queued) {
    return;
  }

  PendingDocumentAction &pending = *bridge->pendingDocumentAction_;
  if (pending.kind == PendingDocumentAction::Kind::Execute) {
    return;
  }
  pending.phase = PendingDocumentAction::Phase::Running;
  AcApDocument *target = bridge->document(pending.documentToken);
  if (!target) {
    pending.commandResult =
        result(acadctl::NativeActionResultKind::DocumentGone);
  } else if (!matchesDatabase(target, pending.databaseToken)) {
    pending.commandResult =
        result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
  } else if (acDocManager->mdiActiveDocument() != target ||
             acDocManager->curDocument() != target) {
    pending.commandResult = nativeFailure(
        acadctl::NativeActionResultKind::ContextFailed,
        Acad::eInvalidContext);
  } else {
    int undoStatus = RTERROR;
    const UndoGroupState undoState = observeUndoGroup(undoStatus);
    if (undoState == UndoGroupState::Active) {
      pending.commandResult =
          result(acadctl::NativeActionResultKind::NotQuiescent);
    } else if (undoState == UndoGroupState::Unknown) {
      pending.commandResult = bridgeFailure(
          acadctl::NativeActionResultKind::HistoryFailed, undoStatus,
          "could not read AutoCAD's undo state");
    } else {
      const int status = acedCommandS(
          RTSTR,
          pending.kind == PendingDocumentAction::Kind::Redo
              ? ACRX_T("_.REDO")
              : ACRX_T("_.U"),
          RTNONE);
      if (acDocManager->mdiActiveDocument() != target ||
          acDocManager->curDocument() != target) {
        pending.commandResult = nativeFailure(
            acadctl::NativeActionResultKind::ContextFailed,
            Acad::eInvalidContext);
      } else if (!matchesDatabase(target, pending.databaseToken)) {
        pending.commandResult =
            result(acadctl::NativeActionResultKind::DocumentGenerationChanged);
      } else if (status != RTNORM) {
        pending.commandResult = bridgeFailure(
            acadctl::NativeActionResultKind::HistoryFailed, status,
            "AutoCAD rejected the history command");
      }
    }
  }

  bridge->queueDocumentActionFinalizer();
}

void ObjectArxBridge::finishDocumentAction(void *) {
  NativeActionCallbackLease callbackLease;
  ObjectArxBridge *bridge = commandBridge_;
  if (!bridge || !bridge->pendingDocumentAction_ ||
      bridge->pendingDocumentAction_->phase !=
          PendingDocumentAction::Phase::Finalizing) {
    return;
  }
  PendingDocumentAction &pending = *bridge->pendingDocumentAction_;

  if (pending.restorePreviousActive) {
    AcApDocument *previousActive =
        bridge->document(pending.previousActiveToken);
    Acad::ErrorStatus restoreStatus = Acad::eNoDocument;
    if (previousActive) {
      restoreStatus = acDocManager->activateDocument(previousActive, false);
    }
    if (restoreStatus != Acad::eOk ||
        acDocManager->mdiActiveDocument() != previousActive ||
        acDocManager->curDocument() != previousActive) {
      pending.commandResult = nativeFailure(
          acadctl::NativeActionResultKind::ContextCleanupFailed,
          restoreStatus == Acad::eOk ? Acad::eInvalidContext : restoreStatus);
    }
  }

  bridge->refreshDocumentSnapshot();
  const std::uint64_t jobId = pending.jobId;
  acadctl::NativeActionResult commandResult = std::move(pending.commandResult);
  bridge->pendingDocumentAction_.reset();
  acadctl::complete_native_action(jobId, std::move(commandResult));
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
  acadctl::replace_documents(std::move(states));
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
  if (!pendingDocumentAction_ ||
      pendingDocumentAction_->phase != PendingDocumentAction::Phase::Queued) {
    return;
  }
  const std::size_t documentToken = static_cast<std::size_t>(
      reinterpret_cast<std::uintptr_t>(document));
  if (pendingDocumentAction_->documentToken != documentToken) {
    return;
  }

  pendingDocumentAction_->commandResult =
      result(acadctl::NativeActionResultKind::DocumentGone);
  queueDocumentActionFinalizer();
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
