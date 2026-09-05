/* Compiled against only the generated SDK header and the platform C SDK. */
#define _CRT_SECURE_NO_WARNINGS
#define WIN32_LEAN_AND_MEAN
#if MODE == 3
#define protogine_plugin_query missing_query
#endif
#include "protogine_plugin.h"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <windows.h>

#ifndef FIXTURE_ID
#define FIXTURE_ID "org.example.a"
#endif
#ifndef MODE
#define MODE 0
#endif
#define STR(s) {(const uint8_t *)(s), (uint32_t)(sizeof(s)-1)}

_Static_assert(sizeof(void *) == 8, "64-bit ABI");
_Static_assert(sizeof(PgStr) == 16 && sizeof(PgError) == 16, "span/error layout");
_Static_assert(sizeof(PgBytes) == 16 && sizeof(PgOutput) == 24, "buffer layout");
_Static_assert(sizeof(PgHost) == 40 && offsetof(PgHost, log) == 16, "host layout");
_Static_assert(sizeof(PgFunction) == 64 && offsetof(PgFunction, call) == 40, "function layout");
_Static_assert(sizeof(PgPlugin) == 72 && _Alignof(PgPlugin) == 8, "plugin layout");
_Static_assert(offsetof(PgPlugin, functions) == 24 && offsetof(PgPlugin, init) == 40 && offsetof(PgPlugin, reserved) == 56, "plugin offsets");

static void trace(const char *phase) {
    const char *path = getenv("PROTOGINE_PLUGIN_TRACE");
    if (path) { FILE *file = fopen(path, "ab"); if (file) { fprintf(file, "%s %s\n", phase, FIXTURE_ID); fclose(file); } }
}
BOOL WINAPI DllMain(HINSTANCE module, DWORD reason, LPVOID reserved) {
    (void)module; (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) trace("load");
    if (reason == DLL_PROCESS_DETACH) trace("unload");
    return TRUE;
}
static uint32_t fail(PgError *error, const char *message) {
    size_t len = strlen(message);
    if (len > error->capacity) len = error->capacity;
    memcpy(error->data, message, len); error->written = (uint32_t)len;
    return PG_ERROR;
}
static DWORD WINAPI wrong_thread(void *opaque) {
    const PgHost *host = (const PgHost *)opaque;
    PgStr message = STR("worker log");
    host->log(host->context, PG_LOG_INFO, message);
    return 0;
}
static uint32_t init(const PgHost *host, void **instance, PgError *error) {
    trace("init");
    if (host->abi_version != PG_ABI_VERSION || host->struct_size != sizeof(PgHost)) return fail(error, "host layout");
    if (MODE == 5) return fail(error, "init failed");
    if (MODE == 14) return PG_OK;
    if (MODE == 15) { static int invalid; *instance = &invalid; return fail(error, "invalid failed init"); }
    *instance = malloc(1);
    if (!*instance) return fail(error, "allocation failed");
    PgStr message = STR("init");
    host->log(host->context, PG_LOG_INFO, message);
    if (MODE == 9) {
        static uint8_t bytes[4096]; memset(bytes, 'x', sizeof(bytes));
        PgStr large = {bytes, sizeof(bytes)};
        for (int i = 0; i < 17; ++i) host->log(host->context, PG_LOG_INFO, large);
    }
    if (MODE == 21) {
        HANDLE worker = CreateThread(NULL, 0, wrong_thread, (void *)host, 0, NULL);
        if (!worker) { free(*instance); *instance = NULL; return fail(error, "thread creation"); }
        WaitForSingleObject(worker, INFINITE); CloseHandle(worker);
    }
    if (MODE == 23) error->written = error->capacity + 1;
    return PG_OK;
}
static uint32_t plugin_shutdown(const PgHost *host, void *instance, PgError *error) {
    trace("shutdown"); free(instance);
    PgStr message = STR("shutdown"); host->log(host->context, PG_LOG_INFO, message);
    return MODE == 6 ? fail(error, "shutdown failed") : PG_OK;
}
static uint32_t batch(const PgHost *host, void *instance, PgBytes input, PgOutput *output, PgError *error) {
    (void)host; (void)instance; (void)input; (void)error;
    output->written = 0; return PG_OK;
}
static PgFunction functions[2] = {
    {sizeof(PgFunction), 1, STR("example.batch"), STR("example.empty"), batch, {0,0}},
    {sizeof(PgFunction), 1, STR("example.batch"), STR("example.empty"), batch, {0,0}},
};
#ifdef USE_HELPER
__declspec(dllimport) int protogine_fixture_helper(void);
#endif

PG_PLUGIN_EXPORT uint32_t protogine_plugin_query(uint32_t abi, uint32_t size, PgPlugin *out, PgError *error) {
    trace("query");
    if (abi != PG_ABI_VERSION || size != sizeof(PgPlugin)) return PG_UNSUPPORTED;
#ifdef USE_HELPER
    if (protogine_fixture_helper() != 7) return fail(error, "wrong dependency selected");
#endif
    PgPlugin descriptor = {PG_ABI_VERSION, sizeof(PgPlugin), STR(FIXTURE_ID), functions, 1, 0, init, plugin_shutdown, {0,0}};
    if (MODE == 1) descriptor.abi_version = 99;
    if (MODE == 2) descriptor.struct_size -= 8;
    if (MODE == 4) descriptor.init = NULL;
    if (MODE == 7) descriptor.function_count = 2;
    if (MODE == 8) functions[0].call = NULL;
    if (MODE == 10) error->written = error->capacity + 1;
    if (MODE == 11) { error->data[0] = 0xff; error->written = 1; return PG_ERROR; }
    if (MODE == 12) functions[0].schema_version = 0;
    if (MODE == 13) descriptor.reserved[0] = 1;
    if (MODE == 17) { descriptor.functions = NULL; descriptor.function_count = 0; }
    if (MODE == 18) descriptor.functions = NULL;
    if (MODE == 19) { PgStr invalid = STR("BAD.ID"); descriptor.id = invalid; }
    if (MODE == 20) error->data = NULL;
    if (MODE == 22) return 999;
    if (MODE == 24) descriptor.flags = 1;
    if (MODE == 25) descriptor.shutdown = NULL;
    if (MODE == 26) {
        /* Keep the full span readable even if the count check regresses. */
        static PgFunction oversized[65];
        for (int i = 0; i < 65; ++i) oversized[i] = functions[0];
        descriptor.functions = oversized; descriptor.function_count = 65;
    }
    if (MODE == 27) descriptor.id.len = 0;
    if (MODE == 28) {
        static uint8_t oversized_id[129];
        memset(oversized_id, 'a', sizeof(oversized_id)); oversized_id[3] = '.';
        descriptor.id.data = oversized_id; descriptor.id.len = sizeof(oversized_id);
    }
    *out = descriptor;
    return PG_OK;
}
