#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include "adscodes.h"
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
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <string>
#include <syslog.h>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);
int acdbSetDbmod(AcDbDatabase *database, int value);
extern "C" int acadctl_wake_native_actions();

namespace {

struct DocumentSubscription {
  AcApDocument *document;
  AcDbDatabase *database;
  bool lispFunctionsDefined;
};

class ObjectArxBridge {
public:
  ObjectArxBridge();

  void start();

  void stop();

  void processPendingActions();

  void setLispFunctionsDefined(AcApDocument *document, bool defined);

private:
  AcApDocument *document(std::size_t token);

  bool lispFunctionsDefined(AcApDocument *document) const;

  acadctl::NativeActionResult open(const rust::String &path);

  acadctl::NativeActionResult save(AcApDocument *document);

  acadctl::NativeActionResult close(AcApDocument *document, bool discard);

  acadctl::NativeActionResult runExecution(AcApDocument *document,
                                            std::size_t databaseToken,
                                            std::uint64_t executionId);

  void publishDocuments();

  void syncDocuments();

  void syncDirtyDocuments();

  void refreshSubscription(DocumentSubscription &subscription);

  void subscribe(AcApDocument *document);

  void unsubscribe(AcApDocument *document);

  class DatabaseReactor final : public AcDbDatabaseReactor {
  public:
    void objectAppended(const AcDbDatabase *, const AcDbObject *) override {
      acadctl::mark_documents_dirty();
    }

    void objectUnAppended(const AcDbDatabase *, const AcDbObject *) override {
      acadctl::mark_documents_dirty();
    }

    void objectReAppended(const AcDbDatabase *, const AcDbObject *) override {
      acadctl::mark_documents_dirty();
    }

    void objectModified(const AcDbDatabase *, const AcDbObject *) override {
      acadctl::mark_documents_dirty();
    }

    void objectErased(const AcDbDatabase *, const AcDbObject *, bool) override {
      acadctl::mark_documents_dirty();
    }

    void headerSysVarChanged(const AcDbDatabase *, const ACHAR *,
                             bool) override {
      acadctl::mark_documents_dirty();
    }
  };

  class DocumentReactor final : public AcApDocManagerReactor {
  public:
    explicit DocumentReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

    void documentCreated(AcApDocument *document) override {
      bridge_.subscribe(document);
      bridge_.syncDocuments();
      acadctl::native_state_may_be_ready();
    }

    void documentToBeDestroyed(AcApDocument *document) override {
      bridge_.unsubscribe(document);
      bridge_.syncDocuments();
      acadctl::native_state_may_be_ready();
    }

    void documentTitleUpdated(AcApDocument *) override {
      bridge_.syncDocuments();
    }

    void documentActivated(AcApDocument *) override {
      bridge_.syncDocuments();
      acadctl::native_state_may_be_ready();
    }

  private:
    ObjectArxBridge &bridge_;
  };

  class EditorReactor final : public AcEditorReactor {
  public:
    explicit EditorReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

    void commandEnded(const ACHAR *) override {
      bridge_.syncDirtyDocuments();
      acadctl::native_state_may_be_ready();
    }

    void commandCancelled(const ACHAR *) override {
      bridge_.syncDirtyDocuments();
      acadctl::native_state_may_be_ready();
    }

    void commandFailed(const ACHAR *) override {
      bridge_.syncDirtyDocuments();
      acadctl::native_state_may_be_ready();
    }

    void lispEnded() override {
      bridge_.syncDirtyDocuments();
      acadctl::native_state_may_be_ready();
    }

    void lispCancelled() override {
      bridge_.syncDirtyDocuments();
      acadctl::native_state_may_be_ready();
    }

    void saveComplete(AcDbDatabase *, const ACHAR *) override {
      bridge_.syncDocuments();
    }

    void abortSave(AcDbDatabase *) override { bridge_.syncDocuments(); }

    void curDocOpenUpgraded(AcDbDatabase *, const CAdUiPathname &) override {
      bridge_.syncDocuments();
    }

    void curDocOpenDowngraded(AcDbDatabase *, const CAdUiPathname &) override {
      bridge_.syncDocuments();
    }

  private:
    ObjectArxBridge &bridge_;
  };

  std::vector<DocumentSubscription> subscriptions_;
  DatabaseReactor databaseReactor_;
  DocumentReactor documentReactor_;
  EditorReactor editorReactor_;
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
  if (status != RTNORM) {
    acedUndef(ACRX_T("acadctl:println"), kPrintlnFunctionCode);
  }
  return status;
}

int undefineLispFunctions() {
  const int privateStatus =
      acedUndef(ACRX_T("acadctl:_value-event"),
                kEvalValueEventFunctionCode);
  const int publicStatus =
      acedUndef(ACRX_T("acadctl:println"), kPrintlnFunctionCode);
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

enum class UndoGroupState { Inactive, Active, Unknown };

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
  result.cleanup_status = cleanupStatus;
  return {std::move(result), reservedStateStillRetained};
}

ReservedStateStepResult evaluateForm(
    rust::Str source, const AcString &evaluatorText, bool retainValue,
    AcApDocument *document, std::size_t databaseToken,
    AcApDocument *expectedActive) {
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
        putStringSymbol(ACRX_T("acadctl:*status*"), pending) != RTNORM) {
      return finishEvaluation(
          stepNativeFailure(RTERROR,
                            "could not stage the AutoLISP form in memory"),
          retainValue);
    }
  }

  const int commandStatus =
      acedCommandS(RTSTR, evaluatorText.kACharPtr(), RTNONE);
  if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
    return {stepNativeFailure(
                RTERROR,
                "the target document context changed during form evaluation"),
            true};
  }
  if (commandStatus != RTNORM) {
    return finishEvaluation(
        stepNativeFailure(commandStatus,
                          "AutoCAD rejected the evaluator expression"),
        retainValue);
  }

  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(ACRX_T("acadctl:*status*"), statusResult);
  if (statusResult != RTNORM || !status) {
    return finishEvaluation(
        stepNativeFailure(statusResult,
                          "the evaluator did not publish a result"),
        retainValue);
  }

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(ACRX_T("acadctl:*errno*"), errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;

  if (status->restype == RTT) {
    return finishEvaluation(stepSuccess(), retainValue);
  }
  if (status->restype != RTNIL) {
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

class EvalValueWriterScope {
public:
  explicit EvalValueWriterScope(acadctl::NativeValueWriter &writer)
      : installed_(activeEvalValueWriter == nullptr) {
    if (installed_) {
      activeEvalValueWriter = &writer;
    }
  }

  ~EvalValueWriterScope() {
    if (installed_) {
      activeEvalValueWriter = nullptr;
    }
  }

  bool installed() const { return installed_; }

private:
  bool installed_;
};

acadctl::NativeExecutionStepResult valueVisitorOutcome(int commandStatus) {
  if (commandStatus != RTNORM) {
    return stepNativeFailure(commandStatus,
                             "AutoCAD rejected the eval value visitor");
  }

  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(ACRX_T("acadctl:*status*"), statusResult);
  if (statusResult != RTNORM || !status) {
    return stepNativeFailure(statusResult,
                             "the eval value visitor did not publish a result");
  }
  if (status->restype == RTT) {
    return stepSuccess();
  }
  if (status->restype != RTNIL) {
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
  result.cleanup_status = cleanupStatus;
  return {std::move(result), reservedStateStillRetained};
}

ReservedStateStepResult emitEvalValue(
    std::uint64_t executionId, AcApDocument *document,
    std::size_t databaseToken, AcApDocument *expectedActive,
    const AcString &visitorText) {
  rust::Box<acadctl::NativeValueWriter> writer =
      acadctl::begin_eval_value(
          executionId,
          static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(document)),
          databaseToken);

  if (!acadctl::value_writer_active(*writer)) {
    acadctl::finish_value_writer(std::move(writer));
    return finishEvalValueEmission(stepSuccess());
  }

  const AcString pending(ACRX_T("pending"));
  int preparationStatus = clearEvaluationSymbols(false);
  if (preparationStatus == RTNORM) {
    preparationStatus =
        putStringSymbol(ACRX_T("acadctl:*status*"), pending);
  }

  acadctl::NativeExecutionStepResult visitorResult = stepSuccess();
  if (preparationStatus != RTNORM) {
    writeValueKind(*writer, acadctl::NativeValueEventKind::Invalid);
    visitorResult = stepNativeFailure(
        preparationStatus, "could not prepare the eval value visitor");
  } else {
    int commandStatus = RTERROR;
    bool visitorInstalled = false;
    {
      EvalValueWriterScope scope(*writer);
      visitorInstalled = scope.installed();
      if (!visitorInstalled) {
        writeValueKind(*writer, acadctl::NativeValueEventKind::Invalid);
      } else {
        commandStatus =
            acedCommandS(RTSTR, visitorText.kACharPtr(), RTNONE);
      }
    }
    if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
      acadctl::finish_value_writer(std::move(writer));
      return {stepNativeFailure(
                  RTERROR,
                  "the target document context changed while emitting the eval value"),
              true};
    }
    visitorResult =
        visitorInstalled
            ? valueVisitorOutcome(commandStatus)
            : stepNativeFailure(RTERROR,
                                "an eval value visitor was already active");
  }

  acadctl::finish_value_writer(std::move(writer));
  return finishEvalValueEmission(std::move(visitorResult));
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
                                    bool ownedGroupStarted) {
  if (!ownedGroupStarted || state == UndoGroupState::Unknown) {
    return {stepNativeFailure(
                RTERROR,
                "the owned undo group could not be identified for rollback"),
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
                "could not prove that rollback closed the owned undo group"),
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
    std::uint64_t executionId, AcApDocument *document,
    std::size_t databaseToken, AcApDocument *expectedActive,
    bool leaseMayBeOpen,
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
      leaseMayBeOpen || reservedStateMayBeRetained;
  if (!acadctl::abandon_execution(
          executionId,
          stepNativeFailure(cleanupStatus == RTNORM ? RTERROR : cleanupStatus,
                            detail))) {
    return bridgeFailure(
        quarantine
            ? acadctl::NativeActionResultKind::ExecutionLeaseFailed
            : acadctl::NativeActionResultKind::ExecutionBridgeFailed,
        RTERROR,
        "Rust could not terminalize an execution after context loss");
  }
  return quarantine
             ? bridgeFailure(
                   acadctl::NativeActionResultKind::ExecutionLeaseFailed,
                   RTERROR,
                   "context loss left native execution state unproved")
             : result(acadctl::NativeActionResultKind::Success);
}

acadctl::NativeActionResult runExecutionSteps(std::uint64_t executionId,
                                              AcApDocument *document,
                                              std::size_t databaseToken,
                                              AcApDocument *expectedActive) {
  UndoGroupState undoGroup = UndoGroupState::Inactive;
  bool ownedGroupStarted = false;
  bool ownedLeaseOpen = false;
  bool formAttempted = false;
  bool reservedStateMayBeRetained = false;
  const rust::Str evaluator = acadctl::execution_evaluator_source();
  const AcString evaluatorText(
      evaluator.data(), AcString::Utf8,
      static_cast<Adesk::UInt32>(evaluator.size()));
  const rust::Str valueVisitor = acadctl::execution_value_source();
  const AcString valueVisitorText(
      valueVisitor.data(), AcString::Utf8,
      static_cast<Adesk::UInt32>(valueVisitor.size()));
  while (true) {
    if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
      return abandonLostExecutionContext(
          executionId, document, databaseToken, expectedActive,
          ownedLeaseOpen,
          reservedStateMayBeRetained);
    }
    rust::Box<acadctl::NativeExecutionStep> step =
        acadctl::take_execution_step(executionId);
    const acadctl::NativeExecutionStepKind kind =
        acadctl::execution_step_kind(*step);
    if (kind == acadctl::NativeExecutionStepKind::Done) {
      int reservedStateCleanupStatus = RTNORM;
      if (reservedStateMayBeRetained) {
        reservedStateCleanupStatus = clearReservedStateIfSafe(
            document, databaseToken, expectedActive,
            reservedStateMayBeRetained);
      }
      if (undoGroup == UndoGroupState::Unknown) {
        acadctl::abandon_execution(
            executionId,
            stepNativeFailure(
                RTERROR, "the execution ended with unknown undo-group state"));
        return bridgeFailure(
            acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
            "the execution ended with unknown undo-group state");
      }
      if (undoGroup == UndoGroupState::Active) {
        UndoCommandResult cleanup =
            formAttempted
                ? rollbackUndoGroup(undoGroup, ownedGroupStarted)
                : runUndoCommand(ACRX_T("_End"),
                                 UndoGroupState::Inactive);
        undoGroup = cleanup.state;
        ownedLeaseOpen = undoGroup != UndoGroupState::Inactive;
        if (formAttempted ||
            cleanup.result.kind !=
                acadctl::NativeExecutionStepResultKind::Success) {
          acadctl::NativeExecutionStepResult terminalFailure =
              cleanup.result.kind ==
                      acadctl::NativeExecutionStepResultKind::Success
                  ? stepNativeFailure(
                        RTERROR,
                        "an unexpected open undo group was rolled back")
                  : std::move(cleanup.result);
          if (!acadctl::abandon_execution(executionId,
                                           std::move(terminalFailure))) {
            return bridgeFailure(
                ownedLeaseOpen
                    ? acadctl::NativeActionResultKind::ExecutionLeaseFailed
                    : acadctl::NativeActionResultKind::ExecutionBridgeFailed,
                RTERROR,
                "Rust could not record emergency undo-group cleanup");
          }
        }
        if (ownedLeaseOpen) {
          return bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
              "the owned undo group could not be closed");
        }
      }
      if (reservedStateMayBeRetained) {
        return bridgeFailure(
            acadctl::NativeActionResultKind::ExecutionStateCleanupFailed,
            reservedStateCleanupStatus == RTNORM ? RTERROR
                                                  : reservedStateCleanupStatus,
            "reserved AutoLISP evaluator state could not be cleared");
      }
      return result(acadctl::NativeActionResultKind::Success);
    }
    if (kind == acadctl::NativeExecutionStepKind::Invalid) {
      if (reservedStateMayBeRetained) {
        const int cleanupStatus = clearReservedStateIfSafe(
            document, databaseToken, expectedActive,
            reservedStateMayBeRetained);
        if (cleanupStatus != RTNORM) {
          acadctl::abandon_execution(
              executionId,
              stepNativeFailure(
                  cleanupStatus,
                  "an invalid execution step left a retained AutoLISP value"));
        }
      }
      if (undoGroup == UndoGroupState::Unknown) {
        return bridgeFailure(
            acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
            "Rust returned an invalid step while undo-group state was unknown");
      }
      if (undoGroup == UndoGroupState::Active) {
        UndoCommandResult cleanup =
            formAttempted
                ? rollbackUndoGroup(undoGroup, ownedGroupStarted)
                : runUndoCommand(ACRX_T("_End"),
                                 UndoGroupState::Inactive);
        if (cleanup.state != UndoGroupState::Inactive) {
          return bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
              "an invalid execution step left the undo group open");
        }
      }
      return bridgeFailure(
          reservedStateMayBeRetained
              ? acadctl::NativeActionResultKind::ExecutionLeaseFailed
              : acadctl::NativeActionResultKind::ExecutionBridgeFailed,
          RTERROR,
          "Rust returned an invalid execution step");
    }

    acadctl::NativeExecutionStepResult stepResult = stepSuccess();
    UndoCommandResult undoTransition{stepSuccess(), undoGroup};
    switch (kind) {
    case acadctl::NativeExecutionStepKind::Begin:
      undoTransition =
          runUndoCommand(ACRX_T("_Begin"), UndoGroupState::Active);
      stepResult = std::move(undoTransition.result);
      break;
    case acadctl::NativeExecutionStepKind::Form:
      formAttempted = true;
      {
        ReservedStateStepResult evaluation = evaluateForm(
            acadctl::execution_step_source(*step), evaluatorText,
            acadctl::execution_step_retain_value(*step), document,
            databaseToken, expectedActive);
        stepResult = std::move(evaluation.result);
        reservedStateMayBeRetained = evaluation.reservedStateRetained;
      }
      break;
    case acadctl::NativeExecutionStepKind::Commit:
      undoTransition =
          runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
      stepResult = std::move(undoTransition.result);
      break;
    case acadctl::NativeExecutionStepKind::EmitValue: {
      ReservedStateStepResult emission = emitEvalValue(
          executionId, document, databaseToken, expectedActive,
          valueVisitorText);
      stepResult = std::move(emission.result);
      reservedStateMayBeRetained = emission.reservedStateRetained;
      break;
    }
    case acadctl::NativeExecutionStepKind::ClearValue: {
      const int cleanupStatus = clearEvaluationSymbols();
      stepResult = cleanupStatus == RTNORM
                       ? stepSuccess()
                       : stepNativeFailure(
                             cleanupStatus,
                             "could not clear the retained AutoLISP value");
      if (cleanupStatus == RTNORM) {
        reservedStateMayBeRetained = false;
      }
      break;
    }
    case acadctl::NativeExecutionStepKind::Abort:
      if (formAttempted) {
        stepResult = stepNativeFailure(
            RTERROR,
            "Rust requested an undo-group close after a form was attempted");
      } else {
        undoTransition =
            runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
        stepResult = std::move(undoTransition.result);
      }
      break;
    case acadctl::NativeExecutionStepKind::Rollback:
      undoTransition = rollbackUndoGroup(undoGroup, ownedGroupStarted);
      stepResult = std::move(undoTransition.result);
      break;
    case acadctl::NativeExecutionStepKind::Invalid:
    case acadctl::NativeExecutionStepKind::Done:
      break;
    }
    if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
      return abandonLostExecutionContext(
          executionId, document, databaseToken, expectedActive,
          ownedLeaseOpen ||
              kind == acadctl::NativeExecutionStepKind::Begin,
          reservedStateMayBeRetained);
    }
    if (kind == acadctl::NativeExecutionStepKind::Form) {
      int observationStatus = RTERROR;
      undoGroup = observeUndoGroup(observationStatus);
      ownedLeaseOpen = undoGroup != UndoGroupState::Inactive;
      if (stepResult.kind ==
              acadctl::NativeExecutionStepResultKind::Success &&
          undoGroup != UndoGroupState::Active) {
        acadctl::NativeExecutionStepResult observationFailure = stepNativeFailure(
            undoGroup == UndoGroupState::Unknown ? observationStatus
                                                 : RTERROR,
            "the owned undo group changed during AutoLISP evaluation");
        observationFailure.cleanup_status = stepResult.cleanup_status;
        stepResult = std::move(observationFailure);
      }
    } else {
      undoGroup = undoTransition.state;
      if (kind == acadctl::NativeExecutionStepKind::Begin) {
        ownedGroupStarted = undoGroup != UndoGroupState::Inactive;
      }
      ownedLeaseOpen = undoGroup != UndoGroupState::Inactive;
    }
    if (!acadctl::complete_execution_step(executionId,
                                           std::move(stepResult))) {
      if (reservedStateMayBeRetained) {
        const int cleanupStatus = clearReservedStateIfSafe(
            document, databaseToken, expectedActive,
            reservedStateMayBeRetained);
        if (cleanupStatus != RTNORM) {
          acadctl::abandon_execution(
              executionId,
              stepNativeFailure(
                  cleanupStatus,
                  "a rejected execution result left a retained AutoLISP value"));
        }
      }
      if (undoGroup == UndoGroupState::Unknown) {
        return bridgeFailure(
            acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
            "Rust rejected a result while undo-group state was unknown");
      }
      if (undoGroup == UndoGroupState::Active) {
        UndoCommandResult cleanup =
            formAttempted
                ? rollbackUndoGroup(undoGroup, ownedGroupStarted)
                : runUndoCommand(ACRX_T("_End"),
                                 UndoGroupState::Inactive);
        if (cleanup.state != UndoGroupState::Inactive) {
          return bridgeFailure(
              acadctl::NativeActionResultKind::ExecutionLeaseFailed, RTERROR,
              "Rust rejected a result and the undo group stayed open");
        }
      }
      return bridgeFailure(
          reservedStateMayBeRetained
              ? acadctl::NativeActionResultKind::ExecutionLeaseFailed
              : acadctl::NativeActionResultKind::ExecutionBridgeFailed,
          RTERROR,
          "Rust rejected an execution step result");
    }
  }
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
    : documentReactor_(*this), editorReactor_(*this) {}

void ObjectArxBridge::start() {
  acDocManager->addReactor(&documentReactor_);
  acedEditor->addReactor(&editorReactor_);

  auto iterator = acDocManager->getDocumentIterator();
  while (!iterator->done()) {
    if (AcApDocument *document = iterator->document()) {
      subscribe(document);
    }
    iterator->step();
  }
  syncDocuments();
}

void ObjectArxBridge::stop() {
  acedEditor->removeReactor(&editorReactor_);
  acDocManager->removeReactor(&documentReactor_);

  for (const DocumentSubscription &subscription : subscriptions_) {
    if (subscription.database) {
      subscription.database->removeReactor(&databaseReactor_);
    }
  }
  subscriptions_.clear();
}

void ObjectArxBridge::processPendingActions() {
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
              : result(acadctl::NativeActionResultKind::DocumentChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DocumentGone);
    }
    break;
  case acadctl::NativeActionKind::Close:
    if (AcApDocument *target = document(action.document_token)) {
      actionResult =
          matchesDatabase(target, action.database_token)
              ? close(target, action.discard)
              : result(acadctl::NativeActionResultKind::DocumentChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DocumentGone);
    }
    break;
  case acadctl::NativeActionKind::RunExecution:
    if (AcApDocument *target = document(action.document_token)) {
      actionResult =
          matchesDatabase(target, action.database_token)
              ? runExecution(target, action.database_token, action.request_id)
              : result(acadctl::NativeActionResultKind::DocumentChanged);
    } else {
      actionResult = result(acadctl::NativeActionResultKind::DocumentGone);
    }
    break;
  case acadctl::NativeActionKind::None:
    return;
  }

  syncDocuments();
  acadctl::complete_native_action(action.request_id, std::move(actionResult));
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

acadctl::NativeActionResult
ObjectArxBridge::runExecution(AcApDocument *document,
                              std::size_t databaseToken,
                              std::uint64_t executionId) {
  if (!lispFunctionsDefined(document)) {
    return bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
        "acadctl AutoLISP functions are unavailable in the target drawing");
  }
  if (!document->isQuiescent()) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }
  if (!document->database()->undoRecording()) {
    return result(acadctl::NativeActionResultKind::UndoDisabled);
  }

  AcApDocument *previousCurrent = acDocManager->curDocument();
  AcApDocument *previousActive = acDocManager->mdiActiveDocument();
  if (!previousActive) {
    return nativeFailure(acadctl::NativeActionResultKind::ContextFailed,
                         Acad::eNoDocument);
  }
  if (previousCurrent != previousActive || !previousActive->isQuiescent()) {
    return result(acadctl::NativeActionResultKind::NotQuiescent);
  }
  const Acad::ErrorStatus lockStatus = acDocManager->lockDocument(
      document, AcAp::kXWrite, nullptr, nullptr, false);
  if (lockStatus != Acad::eOk) {
    return nativeFailure(acadctl::NativeActionResultKind::LockFailed,
                         lockStatus);
  }

  const bool changedCurrent = previousCurrent != document;
  Acad::ErrorStatus setupStatus = Acad::eOk;
  if (changedCurrent) {
    setupStatus =
        acDocManager->setCurDocument(document, AcAp::kNone, false);
  }

  acadctl::NativeActionResult executionResult =
      result(acadctl::NativeActionResultKind::Success);
  if (setupStatus != Acad::eOk) {
    executionResult = nativeFailure(
        acadctl::NativeActionResultKind::ContextFailed, setupStatus);
  } else if (acDocManager->curDocument() != document ||
             acDocManager->mdiActiveDocument() != previousActive) {
    executionResult = nativeFailure(
        acadctl::NativeActionResultKind::ContextFailed,
        Acad::eInvalidContext);
  } else if (!matchesDatabase(document, databaseToken)) {
    executionResult =
        result(acadctl::NativeActionResultKind::DocumentChanged);
  } else if (!document->isQuiescent()) {
    executionResult = result(acadctl::NativeActionResultKind::NotQuiescent);
  } else {
    int commandActivity = 0;
    int commandActivityStatus = RTERROR;
    if (!getIntegerSystemVariable(ACRX_T("CMDACTIVE"), commandActivity,
                                  commandActivityStatus)) {
      executionResult = bridgeFailure(
          acadctl::NativeActionResultKind::ExecutionBridgeFailed,
          commandActivityStatus, "could not read AutoCAD command activity");
    } else {
      int undoStatus = RTERROR;
      const UndoGroupState undoState = observeUndoGroup(undoStatus);
      if (commandActivity != 0 || undoState == UndoGroupState::Active) {
        executionResult =
            result(acadctl::NativeActionResultKind::NotQuiescent);
      } else if (undoState == UndoGroupState::Unknown) {
        executionResult = bridgeFailure(
            acadctl::NativeActionResultKind::ExecutionBridgeFailed,
            undoStatus, "could not read AutoCAD's undo state");
      } else {
        executionResult = runExecutionSteps(executionId, document,
                                            databaseToken, previousActive);
      }
    }
  }

  Acad::ErrorStatus cleanupStatus = Acad::eOk;
  if (changedCurrent) {
    const std::size_t activeToken = static_cast<std::size_t>(
        reinterpret_cast<std::uintptr_t>(previousActive));
    if (this->document(activeToken) == previousActive) {
      const Acad::ErrorStatus restoreStatus =
          acDocManager->setCurDocument(previousActive, AcAp::kNone, false);
      if (cleanupStatus == Acad::eOk) {
        cleanupStatus = restoreStatus;
      }
    } else if (cleanupStatus == Acad::eOk) {
      cleanupStatus = Acad::eNoDocument;
    }
  }
  if ((acDocManager->mdiActiveDocument() != previousActive ||
       acDocManager->curDocument() != previousActive) &&
      cleanupStatus == Acad::eOk) {
    cleanupStatus = Acad::eInvalidContext;
  }
  const Acad::ErrorStatus unlockStatus =
      acDocManager->unlockDocument(document);
  if (cleanupStatus == Acad::eOk) {
    cleanupStatus = unlockStatus;
  }
  if (cleanupStatus != Acad::eOk) {
    return nativeFailure(
        acadctl::NativeActionResultKind::ContextCleanupFailed,
        cleanupStatus);
  }
  return executionResult;
}

void ObjectArxBridge::publishDocuments() {
  rust::Vec<acadctl::NativeDocumentState> states;
  for (DocumentSubscription &subscription : subscriptions_) {
    refreshSubscription(subscription);
    if (!subscription.database) {
      continue;
    }

    AcApDocument *document = subscription.document;
    const bool named = document->isNamedDrawing();
    const AcString name(named ? document->fileName() : document->docTitle());
    states.push_back(acadctl::NativeDocumentState{
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

void ObjectArxBridge::syncDocuments() {
  acadctl::take_documents_dirty();
  publishDocuments();
}

void ObjectArxBridge::syncDirtyDocuments() {
  if (!acadctl::take_documents_dirty()) {
    return;
  }

  publishDocuments();
}

void ObjectArxBridge::refreshSubscription(DocumentSubscription &subscription) {
  AcDbDatabase *database = subscription.document->database();
  if (subscription.database == database) {
    return;
  }

  if (subscription.database) {
    subscription.database->removeReactor(&databaseReactor_);
  }
  subscription.database = database;
  subscription.lispFunctionsDefined = false;
  if (subscription.database) {
    subscription.database->addReactor(&databaseReactor_);
  }
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

  subscriptions_.push_back(DocumentSubscription{document, nullptr, false});
  refreshSubscription(subscriptions_.back());
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

  if (subscription->database) {
    subscription->database->removeReactor(&databaseReactor_);
  }
  subscriptions_.erase(subscription);
}

std::unique_ptr<ObjectArxBridge> objectArxBridge;

void processPendingActions(void *) {
  if (objectArxBridge) {
    objectArxBridge->processPendingActions();
  }
}

} // namespace

extern "C" int acadctl_wake_native_actions() {
  return static_cast<int>(acDocManager->beginExecuteInApplicationContext(
      processPendingActions, nullptr));
}

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                           void *applicationId) {
  switch (message) {
  case AcRx::kInitAppMsg: {
    acrxDynamicLinker->registerAppMDIAware(applicationId);
    objectArxBridge = std::make_unique<ObjectArxBridge>();
    objectArxBridge->start();
    rust::String error = acadctl::start_rpc_server();
    if (!error.empty()) {
      syslog(LOG_ERR, "acadctl plugin failed to start: %s", error.c_str());
      objectArxBridge->stop();
      objectArxBridge.reset();
      return AcRx::kRetError;
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
    acadctl::stop_rpc_server();
    objectArxBridge->stop();
    objectArxBridge.reset();
    break;
  default:
    break;
  }

  return AcRx::kRetOK;
}
