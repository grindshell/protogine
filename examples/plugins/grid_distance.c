/* ABI-only example: see examples/games/native_distance/SCHEMA.md. */
#include "protogine_plugin.h"
#include <stdlib.h>

#define STR(s) {(const uint8_t *)(s), (uint32_t)(sizeof(s)-1)}
static uint32_t read32(const uint8_t *p) {
    return (uint32_t)p[0] | (uint32_t)p[1]<<8 | (uint32_t)p[2]<<16 | (uint32_t)p[3]<<24;
}
static void write32(uint8_t *p, uint32_t v) {
    p[0]=(uint8_t)v; p[1]=(uint8_t)(v>>8); p[2]=(uint8_t)(v>>16); p[3]=(uint8_t)(v>>24);
}
static uint32_t init(const PgHost *host, void **instance, PgError *error) {
    (void)host; (void)error;
    *instance = malloc(1);
    return *instance ? PG_OK : PG_ERROR;
}
static uint32_t shutdown_plugin(const PgHost *host, void *instance, PgError *error) {
    (void)host; (void)error; free(instance); return PG_OK;
}
static void visit(uint32_t at, uint32_t distance, const uint8_t *tiles,
                  uint8_t *out, uint32_t *queue, uint32_t *tail) {
    if (!tiles[at] && read32(out + 4*at) == UINT32_MAX) {
        write32(out + 4*at, distance); queue[(*tail)++] = at;
    }
}
static uint32_t distance_field(const PgHost *host, void *instance, PgBytes input,
                               PgOutput *output, PgError *error) {
    (void)host; (void)instance; (void)error;
    if (input.len < 12) return PG_INVALID_ARGUMENT;
    uint32_t width=read32(input.data), height=read32(input.data+4), source=read32(input.data+8);
    if (!width || !height || width > 256 || height > 256) return PG_INVALID_ARGUMENT;
    uint32_t count=width*height;
    if (input.len != 12+(uint64_t)count || source >= count) return PG_INVALID_ARGUMENT;
    const uint8_t *tiles=input.data+12;
    for (uint32_t i=0; i<count; ++i) if (tiles[i] > 1) return PG_INVALID_ARGUMENT;
    if (tiles[source]) return PG_INVALID_ARGUMENT;
    if (output->capacity < 4*(uint64_t)count) return PG_BUFFER_TOO_SMALL;
    uint32_t *queue=malloc(count*sizeof(uint32_t));
    if (!queue) return PG_ERROR;
    for (uint32_t i=0; i<count; ++i) write32(output->data+4*i, UINT32_MAX);
    uint32_t head=0, tail=1; queue[0]=source; write32(output->data+4*source, 0);
    while (head < tail) {
        uint32_t at=queue[head++], x=at%width, next=read32(output->data+4*at)+1;
        if (x) visit(at-1,next,tiles,output->data,queue,&tail);
        if (x+1 < width) visit(at+1,next,tiles,output->data,queue,&tail);
        if (at >= width) visit(at-width,next,tiles,output->data,queue,&tail);
        if (at+width < count) visit(at+width,next,tiles,output->data,queue,&tail);
    }
    free(queue); output->written=4*(uint64_t)count; return PG_OK;
}
static const PgFunction functions[] = {
    {sizeof(PgFunction), 1, STR("grid.distance"), STR("protogine.grid_distance"), distance_field, {0,0}},
};
PG_PLUGIN_EXPORT uint32_t protogine_plugin_query(uint32_t abi, uint32_t size, PgPlugin *out, PgError *error) {
    (void)error;
    if (abi != PG_ABI_VERSION || size != sizeof(PgPlugin)) return PG_UNSUPPORTED;
    PgPlugin descriptor={PG_ABI_VERSION, sizeof(PgPlugin), STR("org.protogine.grid"),
        functions, 1, 0, init, shutdown_plugin, {0,0}};
    *out=descriptor; return PG_OK;
}
