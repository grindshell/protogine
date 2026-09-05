/* Build using only include/protogine_plugin.h and a C compiler. */
#include "protogine_plugin.h"
#include <stdlib.h>

#define TEXT(s) {(const uint8_t *)(s), (uint32_t)(sizeof(s) - 1)}

static uint32_t plugin_init(const PgHost *host, void **instance, PgError *error) {
    (void)error;
    if (host->abi_version != PG_ABI_VERSION || host->struct_size != sizeof(PgHost) || !host->log)
        return PG_UNSUPPORTED;
    void *state = calloc(1, 1);
    if (!state) return PG_ERROR;
    PgStr message = TEXT("native lifecycle initialized");
    uint32_t status = host->log(host->context, PG_LOG_INFO, message);
    if (status != PG_OK) { free(state); return status; }
    *instance = state;
    return PG_OK;
}

static uint32_t plugin_shutdown(const PgHost *host, void *instance, PgError *error) {
    (void)error;
    free(instance); /* Shutdown consumes the instance even on error. */
    PgStr message = TEXT("native lifecycle stopped");
    return host->log(host->context, PG_LOG_INFO, message);
}

PG_PLUGIN_EXPORT uint32_t protogine_plugin_query(uint32_t abi, uint32_t size, PgPlugin *out, PgError *error) {
    (void)error;
    if (abi != PG_ABI_VERSION || size != sizeof(PgPlugin)) return PG_UNSUPPORTED;
    if (!out) return PG_INVALID_ARGUMENT;
    const PgPlugin descriptor = {
        PG_ABI_VERSION, sizeof(PgPlugin), TEXT("org.protogine.lifecycle"),
        NULL, 0, 0, plugin_init, plugin_shutdown, {0, 0}
    };
    *out = descriptor;
    return PG_OK;
}
