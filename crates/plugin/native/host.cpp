#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include "adscodes.h"
#include "acedCmdNF.h"
#include "acedads.h"
#include "acdocman.h"
#include "aced.h"
#include "acestext.h"
#include "acutads.h"
#include "dbmain.h"
#include "rxregsvc.h"
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <syslog.h>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);
int acdbSetDbmod(AcDbDatabase *database, int value);
extern "C" int acadctl_wake_native_actions();

namespace {

struct DocumentSubscription {
  AcApDocument *document;
  AcDbDatabase *database;
};

class ObjectArxBridge {
public:
  ObjectArxBridge();

  void start();

  void stop();

  void processPendingActions();

private:
  AcApDocument *document(std::size_t token);

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
    }

    void documentToBeDestroyed(AcApDocument *document) override {
      bridge_.unsubscribe(document);
      bridge_.syncDocuments();
    }

    void documentTitleUpdated(AcApDocument *) override {
      bridge_.syncDocuments();
    }

    void documentActivated(AcApDocument *) override { bridge_.syncDocuments(); }

  private:
    ObjectArxBridge &bridge_;
  };

  class EditorReactor final : public AcEditorReactor {
  public:
    explicit EditorReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

    void commandEnded(const ACHAR *) override { bridge_.syncDirtyDocuments(); }

    void commandCancelled(const ACHAR *) override {
      bridge_.syncDirtyDocuments();
    }

    void commandFailed(const ACHAR *) override { bridge_.syncDirtyDocuments(); }

    void lispEnded() override { bridge_.syncDirtyDocuments(); }

    void lispCancelled() override { bridge_.syncDirtyDocuments(); }

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
          rust::String()};
}

acadctl::NativeExecutionStepResult stepNativeFailure(int status,
                                                      const char *detail) {
  return {acadctl::NativeExecutionStepResultKind::NativeError, status, 0,
          rust::String(detail)};
}

struct ResbufDeleter {
  void operator()(resbuf *value) const {
    if (value) {
      acutRelRb(value);
    }
  }
};

using ResbufPtr = std::unique_ptr<resbuf, ResbufDeleter>;

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

int clearEvaluationSymbols() {
  int firstFailure = RTNORM;
  for (const ACHAR *name : {ACRX_T("acadctl:*source*"),
                            ACRX_T("acadctl:*status*"),
                            ACRX_T("acadctl:*error*"),
                            ACRX_T("acadctl:*errno*"),
                            ACRX_T("acadctl:*value*")}) {
    const int status = clearSymbol(name);
    if (firstFailure == RTNORM && status != RTNORM) {
      firstFailure = status;
    }
  }
  return firstFailure;
}

acadctl::NativeExecutionStepResult finishEvaluation(
    acadctl::NativeExecutionStepResult result) {
  const int cleanupStatus = clearEvaluationSymbols();
  return cleanupStatus == RTNORM
             ? std::move(result)
             : stepNativeFailure(
                   cleanupStatus,
                   "could not clear the reserved AutoLISP evaluator state");
}

acadctl::NativeExecutionStepResult evaluateForm(
    rust::Str source, const AcString &evaluatorText) {
  const AcString pending(ACRX_T("pending"));
  const int clearStatus = clearEvaluationSymbols();
  if (clearStatus != RTNORM) {
    return stepNativeFailure(
        clearStatus, "could not clear the reserved AutoLISP evaluator state");
  }
  {
    const AcString form(source.data(), AcString::Utf8,
                        static_cast<Adesk::UInt32>(source.size()));
    if (putStringSymbol(ACRX_T("acadctl:*source*"), form) != RTNORM ||
        putStringSymbol(ACRX_T("acadctl:*status*"), pending) != RTNORM) {
      return finishEvaluation(stepNativeFailure(
          RTERROR, "could not stage the AutoLISP form in memory"));
    }
  }

  const int commandStatus =
      acedCommandS(RTSTR, evaluatorText.kACharPtr(), RTNONE);
  if (commandStatus != RTNORM) {
    return finishEvaluation(stepNativeFailure(
        commandStatus, "AutoCAD rejected the evaluator expression"));
  }

  int statusResult = RTERROR;
  ResbufPtr status = getSymbol(ACRX_T("acadctl:*status*"), statusResult);
  if (statusResult != RTNORM || !status) {
    return finishEvaluation(stepNativeFailure(
        statusResult, "the evaluator did not publish a result"));
  }

  int errnoResult = RTERROR;
  ResbufPtr lispErrno = getSymbol(ACRX_T("acadctl:*errno*"), errnoResult);
  const int lispErrnoValue =
      errnoResult == RTNORM ? integerValue(lispErrno.get()) : 0;

  if (status->restype == RTT) {
    return finishEvaluation(stepSuccess());
  }
  if (status->restype != RTNIL) {
    return finishEvaluation(stepNativeFailure(
        RTERROR, "the evaluator published an invalid result tag"));
  }

  int errorResult = RTERROR;
  ResbufPtr error = getSymbol(ACRX_T("acadctl:*error*"), errorResult);
  rust::String detail("AutoLISP evaluation failed");
  if (errorResult == RTNORM && error && error->restype == RTSTR &&
      error->resval.rstring) {
    const AcString errorText(error->resval.rstring);
    detail = rust::String(errorText.utf8Ptr());
  }
  return finishEvaluation(
      {acadctl::NativeExecutionStepResultKind::LispError, 0, lispErrnoValue,
       std::move(detail)});
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

acadctl::NativeActionResult abandonLostExecutionContext(
    std::uint64_t executionId, bool leaseMayBeOpen) {
  if (!acadctl::abandon_execution(
          executionId,
          stepNativeFailure(
              RTERROR, "the target document context changed during execution"))) {
    return bridgeFailure(
        acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
        "Rust could not terminalize an execution after context loss");
  }
  return leaseMayBeOpen
             ? bridgeFailure(
                   acadctl::NativeActionResultKind::ExecutionLeaseFailed,
                   RTERROR,
                   "context loss obscured an open undo group")
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
  const rust::Str evaluator = acadctl::execution_evaluator_source();
  const AcString evaluatorText(
      evaluator.data(), AcString::Utf8,
      static_cast<Adesk::UInt32>(evaluator.size()));
  while (true) {
    if (!matchesExecutionContext(document, databaseToken, expectedActive)) {
      return abandonLostExecutionContext(executionId, ownedLeaseOpen);
    }
    rust::Box<acadctl::NativeExecutionStep> step =
        acadctl::take_execution_step(executionId);
    const acadctl::NativeExecutionStepKind kind =
        acadctl::execution_step_kind(*step);
    if (kind == acadctl::NativeExecutionStepKind::Done) {
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
                acadctl::NativeActionResultKind::ExecutionBridgeFailed,
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
      return result(acadctl::NativeActionResultKind::Success);
    }
    if (kind == acadctl::NativeExecutionStepKind::Invalid) {
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
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
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
      stepResult = evaluateForm(acadctl::execution_step_source(*step),
                                evaluatorText);
      break;
    case acadctl::NativeExecutionStepKind::Commit:
      undoTransition =
          runUndoCommand(ACRX_T("_End"), UndoGroupState::Inactive);
      stepResult = std::move(undoTransition.result);
      break;
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
          executionId,
          ownedLeaseOpen ||
              kind == acadctl::NativeExecutionStepKind::Begin);
    }
    if (kind == acadctl::NativeExecutionStepKind::Form) {
      int observationStatus = RTERROR;
      undoGroup = observeUndoGroup(observationStatus);
      ownedLeaseOpen = undoGroup != UndoGroupState::Inactive;
      if (stepResult.kind ==
              acadctl::NativeExecutionStepResultKind::Success &&
          undoGroup != UndoGroupState::Active) {
        stepResult = stepNativeFailure(
            undoGroup == UndoGroupState::Unknown ? observationStatus
                                                 : RTERROR,
            "the owned undo group changed during AutoLISP evaluation");
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
          acadctl::NativeActionResultKind::ExecutionBridgeFailed, RTERROR,
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

  subscriptions_.push_back(DocumentSubscription{document, nullptr});
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
