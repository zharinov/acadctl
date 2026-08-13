#include "AcString.h"
#include "aced.h"
#include "rxregsvc.h"

#include "acadctl-plugin/src/lib.rs.h"
#include "dev_reload.h"
#include <dlfcn.h>
#include <mutex>

namespace {

constexpr const ACHAR *kCommandGroup = ACRX_T("ACADCTL_COMMANDS");
using HelloMessage = const char *(*)();

void *payload = nullptr;
HelloMessage helloMessage = nullptr;
std::mutex payloadMutex;

// Rust uses thread-local storage, which dyld otherwise keeps mapped after dlclose.
constexpr int kRtldUnloadable = static_cast<int>(0x80000000);

bool loadPayload() {
    const std::lock_guard lock(payloadMutex);
    if (payload != nullptr) {
        dlclose(payload);
        payload = nullptr;
        helloMessage = nullptr;
    }

    int mode = RTLD_NOW | RTLD_LOCAL;
#ifdef ACADCTL_DEV_RELOAD
    mode |= kRtldUnloadable;
#endif
    void *nextPayload = dlopen(ACADCTL_PAYLOAD_PATH, mode);
    if (nextPayload == nullptr) {
        return false;
    }

    auto nextHelloMessage = reinterpret_cast<HelloMessage>(
        dlsym(nextPayload, "acadctl_hello_message"));
    if (nextHelloMessage == nullptr) {
        dlclose(nextPayload);
        return false;
    }

    payload = nextPayload;
    helloMessage = nextHelloMessage;
    return true;
}

void helloCommand() {
    const std::lock_guard lock(payloadMutex);
    const AcString text(helloMessage == nullptr ? "acadctl payload unavailable"
                                                : helloMessage());
    acutPrintf(ACRX_T("\n%s"), text.kACharPtr());
}

}

namespace acadctl {

void schedule_dev_reload() {
#ifdef ACADCTL_DEV_RELOAD
    loadPayload();
#endif
}

}

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                              void *applicationId) {
    switch (message) {
        case AcRx::kInitAppMsg:
            acrxDynamicLinker->unlockApplication(applicationId);
            acrxDynamicLinker->registerAppMDIAware(applicationId);
            loadPayload();
            acedRegCmds->addCommand(kCommandGroup, ACRX_T("ACADCTL_HELLO"),
                                    ACRX_T("ACADCTL_HELLO"), ACRX_CMD_MODAL,
                                    helloCommand);
#ifdef ACADCTL_DEV_RELOAD
            acadctl::start_dev_watcher(
                rust::String(ACADCTL_RELOAD_SIGNAL_PATH));
#endif
            break;
        case AcRx::kUnloadAppMsg:
#ifdef ACADCTL_DEV_RELOAD
            acadctl::stop_dev_watcher();
#endif
            acedRegCmds->removeGroup(kCommandGroup);
            {
                const std::lock_guard lock(payloadMutex);
                if (payload != nullptr) {
                    dlclose(payload);
                    payload = nullptr;
                    helloMessage = nullptr;
                }
            }
            break;
        default:
            break;
    }

    return AcRx::kRetOK;
}
