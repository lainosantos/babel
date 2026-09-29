#ifndef BABEL_HAL_ABI_H
#define BABEL_HAL_ABI_H
#include <stdint.h>
#include <stddef.h>
/* A private, fixed-width protocol. Actual CoreAudio ABI structs stay in abi.c. */
enum BabelPropertyID { BABEL_PROPERTY_NONE = 0,
#define P(name, sdk) BABEL_PROP_##name,
#include "properties.def"
#undef P
};
typedef struct {
    uint32_t kind;
    uint32_t count;
    uint32_t values[8];
    double number;
    const uint8_t *text;
} BabelProperty;
_Static_assert(sizeof(BabelProperty) == 56, "64-bit Rust/C property layout");
_Static_assert(offsetof(BabelProperty, number) == 40, "Rust/C double alignment");
_Static_assert(offsetof(BabelProperty, text) == 48, "Rust/C pointer alignment");
uint32_t BabelRetain(void);
uint32_t BabelRelease(void);
int32_t BabelInitialize(uint64_t host_time, uint32_t numer, uint32_t denom);
uint8_t BabelValidObject(uint32_t object);
uint8_t BabelHasProperty(uint32_t object, uint32_t property, uint32_t scope, uint32_t element);
uint8_t BabelIsSettable(uint32_t object, uint32_t property);
int32_t BabelGetProperty(uint32_t object, uint32_t property, uint32_t scope, uint32_t element,
                         const uint8_t *qualifier, uint32_t qualifier_size, BabelProperty *result);
int32_t BabelSetActive(uint32_t stream, uint32_t active);
int32_t BabelDeviceAction(uint32_t device, uint32_t client, uint32_t action);
int32_t BabelZeroTimestamp(uint32_t device, uint64_t now, double *sample, uint64_t *host, uint64_t *seed);
int32_t BabelProcess(uint32_t device, uint32_t stream, uint32_t read_input,
                    double sample_time, uint32_t frames, float *data);
#endif
