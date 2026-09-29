// Babel original WDK adapter; MIT.
#include "definitions.h"
#include "BabelTransport.h"

extern "C" void babel_transport_reset(ULONG cable);
extern "C" void babel_transport_state(ULONG cable, ULONG capture, ULONG running);
extern "C" void babel_transport_write(ULONG cable, const BYTE* buffer, SIZE_T bytes);
extern "C" void babel_transport_read(ULONG cable, BYTE* buffer, SIZE_T bytes);
static KSPIN_LOCK g_BabelLocks[2];
static const ULONG kMaxChunk = 4096; // 1024 frames; bounded time at DISPATCH_LEVEL.

#pragma code_seg()
void BabelInitializeTransport() {
    for (ULONG cable=0; cable<2; ++cable) {
        KeInitializeSpinLock(&g_BabelLocks[cable]);
        babel_transport_reset(cable);
    }
}
void BabelSetRunning(ULONG cable, BOOLEAN capture, BOOLEAN running) {
    if (cable >= 2) return;
    KIRQL previous;
    KeAcquireSpinLock(&g_BabelLocks[cable], &previous);
    babel_transport_state(cable, capture ? 1 : 0, running ? 1 : 0);
    KeReleaseSpinLock(&g_BabelLocks[cable], previous);
}
void BabelWrite(ULONG cable, const BYTE* buffer, ULONG bytes) {
    if (cable >= 2 || !buffer || (bytes % 4)) return;
    while (bytes) {
        ULONG chunk = min(bytes,kMaxChunk);
        KIRQL previous;
        KeAcquireSpinLock(&g_BabelLocks[cable], &previous);
        babel_transport_write(cable,buffer,chunk);
        KeReleaseSpinLock(&g_BabelLocks[cable], previous);
        buffer += chunk; bytes -= chunk;
    }
}
void BabelRead(ULONG cable, BYTE* buffer, ULONG bytes) {
    if (cable >= 2 || !buffer || (bytes % 4)) return;
    while (bytes) {
        ULONG chunk = min(bytes,kMaxChunk);
        KIRQL previous;
        KeAcquireSpinLock(&g_BabelLocks[cable], &previous);
        babel_transport_read(cable,buffer,chunk);
        KeReleaseSpinLock(&g_BabelLocks[cable], previous);
        buffer += chunk; bytes -= chunk;
    }
}
extern "C" __declspec(noreturn) void BabelTransportPanic() {
    KeBugCheckEx(DRIVER_CORRUPTED_EXPOOL, 0x42414245, 0, 0, 0);
}
