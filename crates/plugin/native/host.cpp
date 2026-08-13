#include "rxregsvc.h"
#include "acdocman.h"
#include "aced.h"
#include "AcString.h"
#include "dbmain.h"
#include "acadctl-plugin/src/lib.rs.h"
#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <syslog.h>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);

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

private:
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

        void headerSysVarChanged(const AcDbDatabase *, const ACHAR *, bool) override {
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

        void documentActivated(AcApDocument *) override {
            bridge_.syncDocuments();
        }

    private:
        ObjectArxBridge &bridge_;
    };

    class EditorReactor final : public AcEditorReactor {
    public:
        explicit EditorReactor(ObjectArxBridge &bridge) : bridge_(bridge) {}

        void commandEnded(const ACHAR *) override {
            bridge_.syncDirtyDocuments();
        }

        void commandCancelled(const ACHAR *) override {
            bridge_.syncDirtyDocuments();
        }

        void commandFailed(const ACHAR *) override {
            bridge_.syncDirtyDocuments();
        }

        void lispEnded() override {
            bridge_.syncDirtyDocuments();
        }

        void lispCancelled() override {
            bridge_.syncDirtyDocuments();
        }

        void saveComplete(AcDbDatabase *, const ACHAR *) override {
            bridge_.syncDocuments();
        }

        void abortSave(AcDbDatabase *) override {
            bridge_.syncDocuments();
        }

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
            static_cast<std::size_t>(
                reinterpret_cast<std::uintptr_t>(document)),
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
    const auto alreadySubscribed = std::find_if(
        subscriptions_.begin(), subscriptions_.end(),
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
    const auto subscription = std::find_if(
        subscriptions_.begin(), subscriptions_.end(),
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
            objectArxBridge->stop();
            objectArxBridge.reset();
            acadctl::stop_rpc_server();
            break;
        default:
            break;
    }

    return AcRx::kRetOK;
}
