// Host-only stand-in to exercise the actual C++ ABI shim with Rust. Not a WDK.
#pragma once
#include <algorithm>
#include <cstdint>
#include <cstddef>
#include <mutex>
#include <cstdlib>
using ULONG=uint32_t; using BYTE=uint8_t; using BOOLEAN=uint8_t;
using LONG=int32_t; using BOOL=int32_t;
constexpr BOOL FALSE=0;
inline LONG InterlockedCompareExchange(volatile LONG* value,LONG next,LONG expected) {
    __atomic_compare_exchange_n(value,&expected,next,false,__ATOMIC_SEQ_CST,__ATOMIC_SEQ_CST);
    return expected;
}
inline LONG InterlockedExchange(volatile LONG* value,LONG next) {
    return __atomic_exchange_n(value,next,__ATOMIC_SEQ_CST);
}
using SIZE_T=size_t; using KIRQL=unsigned; using KSPIN_LOCK=std::mutex;
#define min std::min
#define __declspec(x) [[x]]
constexpr unsigned DRIVER_CORRUPTED_EXPOOL=0xc5;
inline void KeInitializeSpinLock(KSPIN_LOCK*) {}
inline void KeAcquireSpinLock(KSPIN_LOCK* lock,KIRQL* previous) { *previous=0;lock->lock(); }
inline void KeReleaseSpinLock(KSPIN_LOCK* lock,KIRQL) {lock->unlock();}
[[noreturn]] inline void KeBugCheckEx(unsigned,unsigned,unsigned,unsigned,unsigned) {std::abort();}
