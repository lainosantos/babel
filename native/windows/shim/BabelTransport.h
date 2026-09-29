// Babel original WDK adapter; MIT. All audio transport decisions live in Rust.
#pragma once
void BabelInitializeTransport();
void BabelSetRunning(ULONG cable, BOOLEAN capture, BOOLEAN running);
void BabelWrite(ULONG cable, const BYTE* buffer, ULONG bytes);
void BabelRead(ULONG cable, BYTE* buffer, ULONG bytes);

// Only instantiate from nonpaged code. Matches the lock used by WaveRT's DPC.
class BabelSpinGuard final {
    KSPIN_LOCK* lock_;
    KIRQL previous_;
public:
    explicit BabelSpinGuard(KSPIN_LOCK* lock) : lock_(lock) { KeAcquireSpinLock(lock_, &previous_); }
    ~BabelSpinGuard() { KeReleaseSpinLock(lock_, previous_); }
    BabelSpinGuard(const BabelSpinGuard&) = delete;
    BabelSpinGuard& operator=(const BabelSpinGuard&) = delete;
};
