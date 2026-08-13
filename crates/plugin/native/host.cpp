#include "rxregsvc.h"
#include "acdocman.h"
#include "aced.h"
#include "AcString.h"
#include "acadctl-plugin/src/lib.rs.h"
#include <algorithm>
#include <cctype>
#include <memory>
#include <string>
#include <syslog.h>
#include <utility>
#include <vector>

int acdbGetDbmod(AcDbDatabase *database);

namespace {

struct TrackedDocument {
    AcApDocument *document;
    std::string id;
};

std::string documentPath(AcApDocument *document) {
    const AcString value(document->isNamedDrawing()
        ? document->fileName()
        : document->docTitle());
    std::string path(value.utf8Ptr());
    if (!document->isNamedDrawing() && path.size() >= 4) {
        std::string suffix = path.substr(path.size() - 4);
        std::transform(suffix.begin(), suffix.end(), suffix.begin(),
            [](unsigned char character) { return std::tolower(character); });
        if (suffix == ".dwg") {
            path.resize(path.size() - 4);
        }
    }
    return path;
}

class DocumentRegistry {
public:
    DocumentRegistry();

    void start();

    void stop();

private:
    void publish();

    void track(AcApDocument *document);

    void untrack(AcApDocument *document);

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

    std::vector<TrackedDocument> documents_;
    DocumentReactor documentReactor_;
    EditorReactor editorReactor_;
};

DocumentRegistry::DocumentRegistry()
    : documentReactor_(*this), editorReactor_(*this) {}

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

    documents_.clear();
}

void DocumentRegistry::publish() {
    rust::Vec<acadctl::DocumentState> states;
    for (const TrackedDocument &tracked : documents_) {
        AcApDocument *document = tracked.document;
        if (AcDbDatabase *database = document->database()) {
            states.push_back(acadctl::DocumentState{
                rust::String(tracked.id),
                rust::String(documentPath(document)),
                acdbGetDbmod(database) != 0,
                document->isReadOnly(),
            });
        }
    }
    acadctl::update_documents(std::move(states));
}

void DocumentRegistry::track(AcApDocument *document) {
    const auto alreadyTracked = std::find_if(
        documents_.begin(), documents_.end(),
        [document](const TrackedDocument &tracked) {
            return tracked.document == document;
        });
    if (alreadyTracked != documents_.end()) {
        return;
    }

    std::string id;
    do {
        id = static_cast<std::string>(acadctl::new_document_id());
    } while (std::any_of(
        documents_.begin(), documents_.end(),
        [&id](const TrackedDocument &tracked) { return tracked.id == id; }));
    documents_.push_back(TrackedDocument{document, std::move(id)});
}

void DocumentRegistry::untrack(AcApDocument *document) {
    documents_.erase(
        std::remove_if(
            documents_.begin(), documents_.end(),
            [document](const TrackedDocument &tracked) {
                return tracked.document == document;
            }),
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
