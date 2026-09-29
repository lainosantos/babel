// Host-only stand-in to exercise the actual C++ ABI shim with Rust. Not a WDK.
#pragma once
#include <algorithm>
#include <cstdint>
#include <cstddef>
#include <mutex>
#include <cstdlib>
using ULONG=uint32_t; using BYTE=uint8_t; using BOOLEAN=uint8_t;
using SIZE_T=size_t; using KIRQL=unsigned; using KSPIN_LOCK=std::mutex;
#define min std::min
#define __declspec(x) [[x]]
constexpr unsigned DRIVER_CORRUPTED_EXPOOL=0xc5;
inline void KeInitializeSpinLock(KSPIN_LOCK*) {}
inline void KeAcquireSpinLock(KSPIN_LOCK* lock,KIRQL* previous) { *previous=0;lock->lock(); }
inline void KeReleaseSpinLock(KSPIN_LOCK* lock,KIRQL) {lock->unlock();}
[[noreturn]] inline void KeBugCheckEx(unsigned,unsigned,unsigned,unsigned,unsigned) {std::abort();}
