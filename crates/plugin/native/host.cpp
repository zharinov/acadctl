#include "rxregsvc.h"
#include "acdocman.h"
#include "aced.h"
#include "AcString.h"

#include "acadctl-plugin/src/lib.rs.h"
#include <memory>
#include <syslog.h>

int acdbGetDbmod(AcDbDatabase *database);

namespace {

void publishDocuments() {
    rust::Vec<acadctl::DocumentState> documents;
    auto iterator = acDocManager->getDocumentIterator();
    while (!iterator->done()) {
        if (AcApDocument *document = iterator->document()) {
            const AcString path(document->fileName());
            const bool modified = acdbGetDbmod(document->database()) != 0;
            documents.push_back(acadctl::DocumentState{
                rust::String(path.utf8Ptr()),
                modified,
            });
        }
        iterator->step();
    }
    acadctl::update_documents(std::move(documents));
}

class DocumentReactor final : public AcApDocManagerReactor {
public:
    void documentCreated(AcApDocument *) override {
        publishDocuments();
    }

    void documentDestroyed(const ACHAR *) override {
        publishDocuments();
    }

    void documentTitleUpdated(AcApDocument *) override {
        publishDocuments();
    }
};

class EditorReactor final : public AcEditorReactor {
public:
    void commandEnded(const ACHAR *) override {
        publishDocuments();
    }

    void commandCancelled(const ACHAR *) override {
        publishDocuments();
    }

    void commandFailed(const ACHAR *) override {
        publishDocuments();
    }

    void lispEnded() override {
        publishDocuments();
    }

    void lispCancelled() override {
        publishDocuments();
    }

    void saveComplete(AcDbDatabase *, const ACHAR *) override {
        publishDocuments();
    }
};

std::unique_ptr<DocumentReactor> documentReactor;
std::unique_ptr<EditorReactor> editorReactor;

void startDocumentTracking() {
    documentReactor = std::make_unique<DocumentReactor>();
    acDocManager->addReactor(documentReactor.get());
    editorReactor = std::make_unique<EditorReactor>();
    acedEditor->addReactor(editorReactor.get());
    publishDocuments();
}

void stopDocumentTracking() {
    if (editorReactor) {
        acedEditor->removeReactor(editorReactor.get());
        editorReactor.reset();
    }
    if (documentReactor) {
        acDocManager->removeReactor(documentReactor.get());
        documentReactor.reset();
    }
}

}

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                             void *applicationId) {
    switch (message) {
        case AcRx::kInitAppMsg: {
            acrxDynamicLinker->unlockApplication(applicationId);
            acrxDynamicLinker->registerAppMDIAware(applicationId);
            startDocumentTracking();
            rust::String error = acadctl::start_rpc_server();
            if (!error.empty()) {
                syslog(LOG_ERR, "acadctl plugin failed to start: %s", error.c_str());
                stopDocumentTracking();
                return AcRx::kRetError;
            }
            break;
        }
        case AcRx::kUnloadAppMsg:
            stopDocumentTracking();
            acadctl::stop_rpc_server();
            break;
        default:
            break;
    }

    return AcRx::kRetOK;
}
