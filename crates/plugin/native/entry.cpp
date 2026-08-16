#include "acadctl-plugin/src/lib.rs.h"
#include "acedads.h"
#include "adscodes.h"
#include "host.hpp"
#include "rxregsvc.h"
#include <syslog.h>

extern "C" AcRx::AppRetCode acrxEntryPoint(AcRx::AppMsgCode message,
                                           void* applicationId) {
  switch (message) {
  case AcRx::kInitAppMsg: {
    acrxDynamicLinker->registerAppMDIAware(applicationId);
    try {
      acadctl_create_bridge();
    } catch (...) {
      syslog(LOG_ERR, "acadctl plugin could not allocate its native bridge");

      return AcRx::kRetError;
    }

    const auto failInitialization = []() {
      acadctl_disable_native_wakes();
      acadctl::stop_rpc_server();

      if (acadctl_native_callbacks_outstanding() != 0 ||
          !acadctl_stop_bridge()) {
        syslog(LOG_ERR,
               "acadctl plugin initialization failed after AutoCAD retained "
               "a native callback; the inert module will remain loaded");

        return AcRx::kRetOK;
      }

      acadctl_destroy_bridge();
      return AcRx::kRetError;
    };

    try {
      const Acad::ErrorStatus startStatus = acadctl_start_bridge();

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
    const int status = acadctl_load_doc();

    if (status != RTNORM) {
      syslog(LOG_ERR,
             "acadctl plugin could not define its AutoLISP functions: %d",
             status);
    }

    break;
  }

  case AcRx::kUnloadDwgMsg: {
    const int status = acadctl_unload_doc();

    if (status != RTNORM) {
      syslog(LOG_ERR,
             "acadctl plugin could not undefine its AutoLISP functions: %d",
             status);
    }

    break;
  }

  case AcRx::kUnloadAppMsg:
    acadctl_disable_native_wakes();
    acadctl::stop_rpc_server();

    if (acadctl_native_callbacks_outstanding() != 0) {
      syslog(LOG_ERR,
             "acadctl plugin cannot unload while a native action callback is "
             "outstanding");
      return AcRx::kRetError;
    }

    if (!acadctl_stop_bridge()) {
      syslog(LOG_ERR, "acadctl plugin cannot unload while AutoCAD may retain a "
                      "database reactor");
      return AcRx::kRetError;
    }

    acadctl_destroy_bridge();
    break;
  default:
    break;
  }

  return AcRx::kRetOK;
}
