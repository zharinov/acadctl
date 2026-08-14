#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include "acdocman.h"
#include "aced.h"
#include "acestext.h"
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

bool matchesDatabase(AcApDocument *document, std::size_t databaseToken) {
  return static_cast<std::size_t>(reinterpret_cast<std::uintptr_t>(
             document->database())) == databaseToken;
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
    acrxDynamicLinker->unlockApplication(applicationId);
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
