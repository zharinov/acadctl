#include "rxregsvc.h"
#include "acdocman.h"
#include "aced.h"
#include "AcString.h"
#include "dbmain.h"

#include "acadctl-plugin/src/lib.rs.h"
#include <algorithm>
#include <memory>
#include <syslog.h>
#include <utility>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);

namespace {

class DocumentRegistry {
public:
    DocumentRegistry();

    void start();

    void stop();

private:
    void publish();

    void track(AcApDocument *document);

    void untrack(AcApDocument *document);

    class DatabaseReactor final : public AcDbDatabaseReactor {
    public:
        explicit DatabaseReactor(DocumentRegistry &registry) : registry_(registry) {}

        void objectAppended(const AcDbDatabase *, const AcDbObject *) override {
            registry_.publish();
        }

        void objectUnAppended(const AcDbDatabase *, const AcDbObject *) override {
            registry_.publish();
        }

        void objectReAppended(const AcDbDatabase *, const AcDbObject *) override {
            registry_.publish();
        }

        void objectModified(const AcDbDatabase *, const AcDbObject *) override {
            registry_.publish();
        }

        void objectErased(const AcDbDatabase *, const AcDbObject *, bool) override {
            registry_.publish();
        }

        void headerSysVarChanged(const AcDbDatabase *, const ACHAR *, bool) override {
            registry_.publish();
        }

    private:
        DocumentRegistry &registry_;
    };

    class DocumentReactor final : public AcApDocManagerReactor {
    public:
        explicit DocumentReactor(DocumentRegistry &registry) : registry_(registry) {}

        void documentCreated(AcApDocument *document) override {
            registry_.track(document);
            registry_.publish();
        }

        void documentToBeDestroyed(AcApDocument *document) override {
            registry_.untrack(document);
            registry_.publish();
        }

        void documentTitleUpdated(AcApDocument *) override {
            registry_.publish();
        }

    private:
        DocumentRegistry &registry_;
    };

    class EditorReactor final : public AcEditorReactor {
    public:
        explicit EditorReactor(DocumentRegistry &registry) : registry_(registry) {}

        void commandEnded(const ACHAR *) override {
            registry_.publish();
        }

        void commandCancelled(const ACHAR *) override {
            registry_.publish();
        }

        void commandFailed(const ACHAR *) override {
            registry_.publish();
        }

        void lispEnded() override {
            registry_.publish();
        }

        void lispCancelled() override {
            registry_.publish();
        }

        void saveComplete(AcDbDatabase *, const ACHAR *) override {
            registry_.publish();
        }

    private:
        DocumentRegistry &registry_;
    };

    std::vector<AcApDocument *> documents_;
    DatabaseReactor databaseReactor_;
    DocumentReactor documentReactor_;
    EditorReactor editorReactor_;
};

DocumentRegistry::DocumentRegistry()
    : databaseReactor_(*this), documentReactor_(*this), editorReactor_(*this) {}

void DocumentRegistry::start() {
    acDocManager->addReactor(&documentReactor_);
    acedEditor->addReactor(&editorReactor_);

    auto iterator = acDocManager->getDocumentIterator();
    while (!iterator->done()) {
        if (AcApDocument *document = iterator->document()) {
            track(document);
        }
        iterator->step();
    }
    publish();
}

void DocumentRegistry::stop() {
    acedEditor->removeReactor(&editorReactor_);
    acDocManager->removeReactor(&documentReactor_);

    for (AcApDocument *document : documents_) {
        if (AcDbDatabase *database = document->database()) {
            database->removeReactor(&databaseReactor_);
        }
    }
    documents_.clear();
}

void DocumentRegistry::publish() {
    rust::Vec<acadctl::DocumentState> states;
    for (AcApDocument *document : documents_) {
        if (AcDbDatabase *database = document->database()) {
            const AcString path(document->fileName());
            states.push_back(acadctl::DocumentState{
                rust::String(path.utf8Ptr()),
                acdbGetDbmod(database) != 0,
            });
        }
    }
    acadctl::update_documents(std::move(states));
}

void DocumentRegistry::track(AcApDocument *document) {
    if (std::find(documents_.begin(), documents_.end(), document) != documents_.end()) {
        return;
    }
    documents_.push_back(document);
    if (AcDbDatabase *database = document->database()) {
        database->addReactor(&databaseReactor_);
    }
}

void DocumentRegistry::untrack(AcApDocument *document) {
    if (AcDbDatabase *database = document->database()) {
        database->removeReactor(&databaseReactor_);
    }
    documents_.erase(
        std::remove(documents_.begin(), documents_.end(), document),
        documents_.end());
}

std::unique_ptr<DocumentRegistry> documentRegistry;

}

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                             void *applicationId) {
    switch (message) {
        case AcRx::kInitAppMsg: {
            acrxDynamicLinker->unlockApplication(applicationId);
            acrxDynamicLinker->registerAppMDIAware(applicationId);
            documentRegistry = std::make_unique<DocumentRegistry>();
            documentRegistry->start();
            rust::String error = acadctl::start_rpc_server();
            if (!error.empty()) {
                syslog(LOG_ERR, "acadctl plugin failed to start: %s", error.c_str());
                documentRegistry->stop();
                documentRegistry.reset();
                return AcRx::kRetError;
            }
            break;
        }
        case AcRx::kUnloadAppMsg:
            documentRegistry->stop();
            documentRegistry.reset();
            acadctl::stop_rpc_server();
            break;
        default:
            break;
    }

    return AcRx::kRetOK;
}
