#include "rxregsvc.h"
#include "acdocman.h"
#include "aced.h"
#include "AcString.h"
#include "dbmain.h"
#include "acadctl-plugin/src/lib.rs.h"
#include <algorithm>
#include <cctype>
#include <list>
#include <memory>
#include <string>
#include <syslog.h>
#include <unordered_map>
#include <utility>

int acdbGetDbmod(AcDbDatabase *database);

namespace {

struct TrackedDocument {
    AcApDocument *document;
    AcDbDatabase *database;
    std::string id;
    std::string path;
    bool modified;
    bool readOnly;
    bool dirty;
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

    void flush();

    void flush(const AcDbDatabase *database);

    void flush(AcApDocument *document);

    void markDirty(const AcDbDatabase *database);

    void markDirty(AcApDocument *document);

    bool refresh(TrackedDocument &tracked);

    void track(AcApDocument *document);

    void untrack(AcApDocument *document);

    class DatabaseReactor final : public AcDbDatabaseReactor {
    public:
        explicit DatabaseReactor(DocumentRegistry &registry) : registry_(registry) {}

        void objectAppended(const AcDbDatabase *database, const AcDbObject *) override {
            registry_.markDirty(database);
        }

        void objectUnAppended(const AcDbDatabase *database, const AcDbObject *) override {
            registry_.markDirty(database);
        }

        void objectReAppended(const AcDbDatabase *database, const AcDbObject *) override {
            registry_.markDirty(database);
        }

        void objectModified(const AcDbDatabase *database, const AcDbObject *) override {
            registry_.markDirty(database);
        }

        void objectErased(const AcDbDatabase *database, const AcDbObject *, bool) override {
            registry_.markDirty(database);
        }

        void headerSysVarChanged(const AcDbDatabase *database, const ACHAR *, bool) override {
            registry_.markDirty(database);
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

        void documentTitleUpdated(AcApDocument *document) override {
            registry_.flush(document);
        }

        void documentActivated(AcApDocument *document) override {
            registry_.flush(document);
        }

    private:
        DocumentRegistry &registry_;
    };

    class EditorReactor final : public AcEditorReactor {
    public:
        explicit EditorReactor(DocumentRegistry &registry) : registry_(registry) {}

        void commandEnded(const ACHAR *) override {
            registry_.flush();
        }

        void commandCancelled(const ACHAR *) override {
            registry_.flush();
        }

        void commandFailed(const ACHAR *) override {
            registry_.flush();
        }

        void lispEnded() override {
            registry_.flush();
        }

        void lispCancelled() override {
            registry_.flush();
        }

        void saveComplete(AcDbDatabase *database, const ACHAR *) override {
            registry_.flush(database);
        }

        void abortSave(AcDbDatabase *database) override {
            registry_.flush(database);
        }

        void curDocOpenUpgraded(AcDbDatabase *database, const CAdUiPathname &) override {
            registry_.flush(database);
        }

        void curDocOpenDowngraded(AcDbDatabase *database, const CAdUiPathname &) override {
            registry_.flush(database);
        }

    private:
        DocumentRegistry &registry_;
    };

    std::list<TrackedDocument> documents_;
    std::unordered_map<const AcDbDatabase *, TrackedDocument *> documentsByDatabase_;
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

    for (const TrackedDocument &tracked : documents_) {
        if (tracked.database) {
            tracked.database->removeReactor(&databaseReactor_);
        }
    }
    documentsByDatabase_.clear();
    documents_.clear();
}

void DocumentRegistry::publish() {
    rust::Vec<acadctl::DocumentState> states;
    for (const TrackedDocument &tracked : documents_) {
        if (tracked.database) {
            states.push_back(acadctl::DocumentState{
                rust::String(tracked.id),
                rust::String(tracked.path),
                tracked.modified,
                tracked.readOnly,
            });
        }
    }
    acadctl::update_documents(std::move(states));
}

void DocumentRegistry::flush() {
    bool changed = false;
    for (TrackedDocument &tracked : documents_) {
        if (tracked.dirty) {
            changed = refresh(tracked) || changed;
        }
    }
    if (changed) {
        publish();
    }
}

void DocumentRegistry::flush(const AcDbDatabase *database) {
    markDirty(database);
    flush();
}

void DocumentRegistry::flush(AcApDocument *document) {
    markDirty(document);
    flush();
}

void DocumentRegistry::markDirty(const AcDbDatabase *database) {
    const auto tracked = documentsByDatabase_.find(database);
    if (tracked != documentsByDatabase_.end()) {
        tracked->second->dirty = true;
    }
}

void DocumentRegistry::markDirty(AcApDocument *document) {
    const auto tracked = std::find_if(
        documents_.begin(), documents_.end(),
        [document](const TrackedDocument &candidate) {
            return candidate.document == document;
        });
    if (tracked != documents_.end()) {
        tracked->dirty = true;
    }
}

bool DocumentRegistry::refresh(TrackedDocument &tracked) {
    AcDbDatabase *database = tracked.document->database();
    const bool databaseChanged = tracked.database != database;
    if (tracked.database != database) {
        if (tracked.database) {
            tracked.database->removeReactor(&databaseReactor_);
            documentsByDatabase_.erase(tracked.database);
        }
        tracked.database = database;
        if (tracked.database) {
            tracked.database->addReactor(&databaseReactor_);
            documentsByDatabase_[tracked.database] = &tracked;
        }
    }

    std::string path = documentPath(tracked.document);
    const bool modified = tracked.database && acdbGetDbmod(tracked.database) != 0;
    const bool readOnly = tracked.document->isReadOnly();
    const bool changed = databaseChanged
        || tracked.path != path
        || tracked.modified != modified
        || tracked.readOnly != readOnly;
    tracked.path = std::move(path);
    tracked.modified = modified;
    tracked.readOnly = readOnly;
    tracked.dirty = false;
    return changed;
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
    documents_.push_back(TrackedDocument{
        document,
        nullptr,
        std::move(id),
        {},
        false,
        false,
        true,
    });
    refresh(documents_.back());
}

void DocumentRegistry::untrack(AcApDocument *document) {
    const auto tracked = std::find_if(
        documents_.begin(), documents_.end(),
        [document](const TrackedDocument &candidate) {
            return candidate.document == document;
        });
    if (tracked != documents_.end() && tracked->database) {
        tracked->database->removeReactor(&databaseReactor_);
        documentsByDatabase_.erase(tracked->database);
    }
    if (tracked != documents_.end()) {
        documents_.erase(tracked);
    }
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
