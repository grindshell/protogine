/* Phase 0 C/C++ compiler and target probe; this is not the plugin ABI. */
#include <stdint.h>
#include <stddef.h>

struct Probe { uint32_t version; uint32_t size; void *context; };
#ifdef __cplusplus
static_assert(sizeof(void *) == 8, "64-bit target required");
static_assert(offsetof(Probe, context) == 8, "unexpected layout");
#else
_Static_assert(sizeof(void *) == 8, "64-bit target required");
_Static_assert(offsetof(struct Probe, context) == 8, "unexpected layout");
#endif

int main(void) {
    struct Probe probe = { 1, sizeof(struct Probe), 0 };
    return probe.size == 16 && probe.version == 1 ? 0 : 1;
}
