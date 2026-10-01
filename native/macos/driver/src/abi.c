/* The SDK owns all HAL/COM/CoreFoundation layouts. The Rust core owns audio and
 * object state. This shim does no allocation, locking or logging in IO callbacks.
 * Interface contract checked against Apple's NullAudio sample (2024, MIT).
 */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreAudio/AudioHardware.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <mach/mach_time.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <math.h>
#include <string.h>
#include "abi.h"

_Static_assert(kAudioObjectClassID == 0x616f626a, "Rust object class");
_Static_assert(kAudioPlugInClassID == 0x61706c67, "Rust plugin class");
_Static_assert(kAudioBoxClassID == 0x61626f78, "Rust box class");
_Static_assert(kAudioDeviceClassID == 0x61646576, "Rust device class");
_Static_assert(kAudioStreamClassID == 0x61737472, "Rust stream class");
_Static_assert(kAudioVolumeControlClassID == 0x766c6d65, "Rust volume class");
_Static_assert(kAudioMuteControlClassID == 0x6d757465, "Rust mute class");
_Static_assert(kAudioLevelControlClassID == 0x6c65766c, "Rust level base class");
_Static_assert(kAudioBooleanControlClassID == 0x746f676c, "Rust boolean base class");

static AudioServerPlugInDriverInterface interface;
static AudioServerPlugInDriverInterface *interface_pointer = &interface;
static AudioServerPlugInDriverRef driver_reference = &interface_pointer;
static _Atomic(AudioServerPlugInHostRef) host_reference;

static OSStatus status(int32_t result) {
    switch(result) {
        case 0: return kAudioHardwareNoError;
        case -1: return kAudioHardwareBadObjectError;
        case -2: return kAudioHardwareUnknownPropertyError;
        default: return kAudioHardwareIllegalOperationError;
    }
}
static bool valid_driver(AudioServerPlugInDriverRef driver) { return driver == driver_reference; }
static bool valid_device(AudioObjectID device) { return device == 10 || device == 20; }
static uint32_t scope_id(AudioObjectPropertyScope scope) {
    if(scope == kAudioObjectPropertyScopeGlobal) return 0;
    if(scope == kAudioObjectPropertyScopeInput) return 1;
    if(scope == kAudioObjectPropertyScopeOutput) return 2;
    return UINT32_MAX;
}
static uint32_t property_id(AudioObjectID object, const AudioObjectPropertyAddress *address) {
    if(!address) return 0;
    /* Some SDK selectors alias each other (e.g. device/stream latency); choose
     * the first matching selector supported by this particular object. */
#define P(name, sdk) if(address->mSelector == sdk && BabelHasProperty(object, BABEL_PROP_##name, scope_id(address->mScope), address->mElement)) return BABEL_PROP_##name;
#include "properties.def"
#undef P
    return 0;
}
static void notify(AudioObjectID object, AudioObjectPropertySelector selector) {
    AudioServerPlugInHostRef host = atomic_load_explicit(&host_reference, memory_order_acquire);
    if(host) {
        const AudioObjectPropertyAddress address = {selector, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
        host->PropertiesChanged(host, object, 1, &address);
    }
}
static void notify_output_control(bool mute) {
    AudioServerPlugInHostRef host = atomic_load_explicit(&host_reference, memory_order_acquire);
    if(!host) return;
    const AudioObjectPropertyAddress control[] = {
        {mute ? kAudioBooleanControlPropertyValue : kAudioLevelControlPropertyScalarValue, kAudioObjectPropertyScopeGlobal, 0},
        {kAudioLevelControlPropertyDecibelValue, kAudioObjectPropertyScopeGlobal, 0}
    };
    const AudioObjectPropertyAddress device[] = {
        {mute ? kAudioDevicePropertyMute : kAudioDevicePropertyVolumeScalar, kAudioObjectPropertyScopeOutput, 0},
        {kAudioDevicePropertyVolumeDecibels, kAudioObjectPropertyScopeOutput, 0}
    };
    host->PropertiesChanged(host, mute ? 24 : 23, mute ? 1 : 2, control);
    host->PropertiesChanged(host, 20, mute ? 1 : 2, device);
}
static HRESULT query(void *driver, REFIID uuid, LPVOID *result) {
    if(driver != driver_reference || !result) return E_NOINTERFACE;
    *result = NULL;
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(NULL, uuid);
    if(!requested) return E_NOINTERFACE;
    bool matches = CFEqual(requested, IUnknownUUID) || CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID);
    CFRelease(requested);
    if(!matches) return E_NOINTERFACE;
    BabelRetain(); *result = driver_reference; return S_OK;
}
static ULONG retain(void *driver) { return driver == driver_reference ? BabelRetain() : 0; }
static ULONG release(void *driver) { return driver == driver_reference ? BabelRelease() : 0; }
static OSStatus initialize(AudioServerPlugInDriverRef driver, AudioServerPlugInHostRef host) {
    if(!valid_driver(driver) || !host) return kAudioHardwareBadObjectError;
    mach_timebase_info_data_t timebase;
    if(mach_timebase_info(&timebase) != KERN_SUCCESS) return kAudioHardwareUnspecifiedError;
    OSStatus result = status(BabelInitialize(mach_absolute_time(),timebase.numer,timebase.denom));
    if(result == noErr) atomic_store_explicit(&host_reference,host,memory_order_release);
    return result;
}
static OSStatus create_device(AudioServerPlugInDriverRef driver, CFDictionaryRef description, const AudioServerPlugInClientInfo *client, AudioObjectID *device) {
    if(device) *device = kAudioObjectUnknown;
    return kAudioHardwareUnsupportedOperationError;
}
static OSStatus destroy_device(AudioServerPlugInDriverRef driver, AudioObjectID device) { return kAudioHardwareUnsupportedOperationError; }
static OSStatus add_client(AudioServerPlugInDriverRef driver, AudioObjectID device, const AudioServerPlugInClientInfo *client) {
    if(!valid_driver(driver) || !client) return kAudioHardwareBadObjectError;
    return status(BabelDeviceAction(device,client->mClientID,0));
}
static OSStatus remove_client(AudioServerPlugInDriverRef driver, AudioObjectID device, const AudioServerPlugInClientInfo *client) {
    if(!valid_driver(driver) || !client) return kAudioHardwareBadObjectError;
    OSStatus result = status(BabelDeviceAction(device,client->mClientID,1));
    if(result == noErr) notify(device,kAudioDevicePropertyDeviceIsRunning);
    return result;
}
static OSStatus configure(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt64 action, void *info) {
    /* Rate/format are immutable: this driver never requests a configuration change. */
    return valid_driver(driver) && valid_device(device) ? kAudioHardwareUnsupportedOperationError : kAudioHardwareBadObjectError;
}
static Boolean has_property(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t pid, const AudioObjectPropertyAddress *address) {
    return valid_driver(driver) && property_id(object,address) != 0;
}
static OSStatus is_settable(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t pid, const AudioObjectPropertyAddress *address, Boolean *result) {
    if(!valid_driver(driver) || !BabelValidObject(object)) return kAudioHardwareBadObjectError;
    if(!address || !result) return kAudioHardwareIllegalOperationError;
    uint32_t property = property_id(object,address);
    if(!property) return kAudioHardwareUnknownPropertyError;
    *result = BabelIsSettable(object,property); return noErr;
}
static OSStatus get_value(AudioServerPlugInDriverRef driver, AudioObjectID object, const AudioObjectPropertyAddress *address, UInt32 qualifier_size, const void *qualifier, bool need_qualifier, BabelProperty *value) {
    if(!valid_driver(driver) || !BabelValidObject(object)) return kAudioHardwareBadObjectError;
    if(!address) return kAudioHardwareIllegalOperationError;
    uint32_t property = property_id(object,address);
    if(!property) return kAudioHardwareUnknownPropertyError;
    char text[512] = {0}; uint32_t length = 0;
    if(need_qualifier && (property == BABEL_PROP_UID_TO_BOX || property == BABEL_PROP_UID_TO_DEVICE)) {
        if(qualifier_size != sizeof(CFStringRef) || !qualifier) return kAudioHardwareBadPropertySizeError;
        CFStringRef string; memcpy(&string,qualifier,sizeof(string));
        if(!string || CFGetTypeID(string) != CFStringGetTypeID() || !CFStringGetCString(string,text,sizeof(text),kCFStringEncodingUTF8)) return kAudioHardwareIllegalOperationError;
        length = (uint32_t)strlen(text);
    }
    OSStatus result = status(BabelGetProperty(object,property,scope_id(address->mScope),address->mElement,(const uint8_t *)text,length,value));
    /* OwnedObjects optionally filters by an array of AudioClassIDs. */
    if(result == noErr && property == BABEL_PROP_OWNED_OBJECTS && qualifier_size != 0) {
        if(!qualifier || qualifier_size % sizeof(AudioClassID) != 0 || qualifier_size > 1024) return kAudioHardwareBadPropertySizeError;
        uint32_t count = 0;
        for(uint32_t item = 0; item < value->count; ++item) {
            BabelProperty child;
            if(BabelGetProperty(value->values[item],BABEL_PROP_CLASS,0,0,NULL,0,&child) != 0) continue;
            bool included = false;
            for(uint32_t i = 0; i < qualifier_size / sizeof(AudioClassID); ++i) {
                AudioClassID filter; memcpy(&filter,(const uint8_t *)qualifier+i*sizeof(filter),sizeof(filter));
                if(filter == child.values[0] || filter == kAudioObjectClassID) included = true;
                if(child.values[0] == kAudioVolumeControlClassID && (filter == kAudioLevelControlClassID || filter == kAudioControlClassID)) included = true;
                if(child.values[0] == kAudioMuteControlClassID && (filter == kAudioBooleanControlClassID || filter == kAudioControlClassID)) included = true;
            }
            if(included) value->values[count++] = value->values[item];
        }
        value->count = count;
    }
    return result;
}
static UInt32 value_size(const BabelProperty *value) {
    switch(value->kind) {
        case 1: case 9: case 10: case 13: return sizeof(UInt32);
        case 2: return value->count * sizeof(UInt32);
        case 3: return sizeof(Float64);
        case 4: return sizeof(CFStringRef);
        case 5: case 12: return sizeof(AudioValueRange);
        case 11: case 14: return sizeof(Float32);
        case 6: return sizeof(AudioStreamBasicDescription);
        case 7: return sizeof(AudioStreamRangedDescription);
        case 8: return offsetof(AudioChannelLayout,mChannelDescriptions) + 2*sizeof(AudioChannelDescription);
        default: return 0;
    }
}
static OSStatus get_size(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t pid, const AudioObjectPropertyAddress *address, UInt32 qualifier_size, const void *qualifier, UInt32 *size) {
    if(!size) return kAudioHardwareIllegalOperationError;
    BabelProperty value; OSStatus result=get_value(driver,object,address,qualifier_size,qualifier,false,&value);
    *size = result == noErr ? value_size(&value) : 0; return result;
}
static AudioStreamBasicDescription format(void) {
    const AudioStreamBasicDescription result = {48000.0,kAudioFormatLinearPCM,kAudioFormatFlagsNativeFloatPacked,8,1,8,2,32,0};
    return result;
}
static OSStatus get_data(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t pid, const AudioObjectPropertyAddress *address, UInt32 qualifier_size, const void *qualifier, UInt32 capacity, UInt32 *size, void *data) {
    if(!size) return kAudioHardwareIllegalOperationError;
    *size = 0;
    BabelProperty value; OSStatus result=get_value(driver,object,address,qualifier_size,qualifier,true,&value);
    if(result != noErr) return result;
    UInt32 required=value_size(&value);
    if(value.kind == 2 && capacity < required) required=(capacity/sizeof(UInt32))*sizeof(UInt32);
    if(capacity < required || (required && !data)) return kAudioHardwareBadPropertySizeError;
    switch(value.kind) {
        case 1: case 2: if(required) memcpy(data,value.values,required); break;
        case 3: memcpy(data,&value.number,required); break;
        case 4: {
            CFStringRef string=CFStringCreateWithBytes(NULL,value.text,value.count,kCFStringEncodingUTF8,false);
            if(!string) return kAudioHardwareUnspecifiedError;
            memcpy(data,&string,sizeof(string)); break;
        }
        case 5: { const AudioValueRange rates={48000.0,48000.0}; memcpy(data,&rates,sizeof(rates)); break; }
        case 6: { const AudioStreamBasicDescription description=format(); memcpy(data,&description,sizeof(description)); break; }
        case 7: { const AudioStreamRangedDescription description={format(),{48000.0,48000.0}}; memcpy(data,&description,sizeof(description)); break; }
        case 8: {
            /* SDK struct has one trailing description; use aligned fixed storage
             * large enough for both channels, without heap allocation. */
            union { AudioChannelLayout alignment; uint8_t bytes[offsetof(AudioChannelLayout,mChannelDescriptions)+2*sizeof(AudioChannelDescription)]; } storage;
            memset(&storage,0,sizeof(storage));
            AudioChannelLayout *layout=(AudioChannelLayout *)storage.bytes;
            layout->mChannelLayoutTag=kAudioChannelLayoutTag_UseChannelDescriptions;
            layout->mNumberChannelDescriptions=2;
            AudioChannelDescription channels[2]={{kAudioChannelLabel_Left,0,{0,0,0}},{kAudioChannelLabel_Right,0,{0,0,0}}};
            memcpy(storage.bytes+offsetof(AudioChannelLayout,mChannelDescriptions),channels,sizeof(channels));
            memcpy(data,storage.bytes,required); break;
        }
        case 9: { const UInt32 transport=kAudioDeviceTransportTypeVirtual; memcpy(data,&transport,sizeof(transport)); break; }
        case 10: { const UInt32 terminal=kAudioStreamTerminalTypeLine; memcpy(data,&terminal,sizeof(terminal)); break; }
        case 11: { const Float32 scalar=(Float32)value.number; memcpy(data,&scalar,sizeof(scalar)); break; }
        case 12: { const AudioValueRange range={-96.0,0.0}; memcpy(data,&range,sizeof(range)); break; }
        case 13: { const UInt32 scope=kAudioObjectPropertyScopeOutput; memcpy(data,&scope,sizeof(scope)); break; }
        case 14: {
            Float32 scalar; memcpy(&scalar,data,sizeof(scalar));
            if(!isfinite(scalar)) return kAudioHardwareIllegalOperationError;
            scalar=BabelConvertLevel(address->mSelector == kAudioLevelControlPropertyConvertScalarToDecibels,scalar);
            memcpy(data,&scalar,sizeof(scalar)); break;
        }
        default: return kAudioHardwareUnknownPropertyError;
    }
    *size=required; return noErr;
}
static bool valid_format(const AudioStreamBasicDescription *value) {
    AudioStreamBasicDescription expected=format();
    return value->mSampleRate==expected.mSampleRate && value->mFormatID==expected.mFormatID && value->mFormatFlags==expected.mFormatFlags
        && value->mBytesPerPacket==expected.mBytesPerPacket && value->mFramesPerPacket==expected.mFramesPerPacket
        && value->mBytesPerFrame==expected.mBytesPerFrame && value->mChannelsPerFrame==expected.mChannelsPerFrame && value->mBitsPerChannel==expected.mBitsPerChannel;
}
static OSStatus set_data(AudioServerPlugInDriverRef driver, AudioObjectID object, pid_t pid, const AudioObjectPropertyAddress *address, UInt32 qualifier_size, const void *qualifier, UInt32 size, const void *data) {
    if(!valid_driver(driver) || !BabelValidObject(object)) return kAudioHardwareBadObjectError;
    if(!address || !data) return kAudioHardwareIllegalOperationError;
    uint32_t property=property_id(object,address);
    if(!property) return kAudioHardwareUnknownPropertyError;
    if(!BabelIsSettable(object,property)) return kAudioHardwareUnsupportedOperationError;
    if(property == BABEL_PROP_LEVEL_SCALAR || property == BABEL_PROP_LEVEL_DECIBELS || property == BABEL_PROP_DEVICE_VOLUME || property == BABEL_PROP_DEVICE_DECIBELS) {
        if(size != sizeof(Float32)) return kAudioHardwareBadPropertySizeError;
        Float32 value; memcpy(&value,data,sizeof(value));
        int32_t result=BabelSetLevel(object,property,value);
        if(result<0) return status(result);
        if(result>0) notify_output_control(false);
        return noErr;
    }
    if(property == BABEL_PROP_BOOLEAN_VALUE || property == BABEL_PROP_DEVICE_MUTE) {
        if(size != sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
        UInt32 value; memcpy(&value,data,sizeof(value));
        int32_t result=BabelSetMute(object,value);
        if(result<0) return status(result);
        if(result>0) notify_output_control(true);
        return noErr;
    }
    if(property == BABEL_PROP_SAMPLE_RATE) {
        if(size!=sizeof(Float64)) return kAudioHardwareBadPropertySizeError;
        Float64 rate; memcpy(&rate,data,sizeof(rate)); return rate==48000.0 ? noErr : kAudioHardwareIllegalOperationError;
    }
    if(property == BABEL_PROP_VIRTUAL_FORMAT || property == BABEL_PROP_PHYSICAL_FORMAT) {
        if(size!=sizeof(AudioStreamBasicDescription)) return kAudioHardwareBadPropertySizeError;
        AudioStreamBasicDescription description; memcpy(&description,data,sizeof(description));
        return valid_format(&description) ? noErr : kAudioDeviceUnsupportedFormatError;
    }
    if(size!=sizeof(UInt32)) return kAudioHardwareBadPropertySizeError;
    UInt32 value; memcpy(&value,data,sizeof(value));
    if(property == BABEL_PROP_ACQUIRED) return value==1 ? noErr : kAudioHardwareUnsupportedOperationError;
    int32_t result=BabelSetActive(object,value);
    if(result<0) return status(result);
    if(result>0) notify(object,kAudioStreamPropertyIsActive);
    return noErr;
}
static OSStatus start_io(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client) {
    if(!valid_driver(driver)) return kAudioHardwareBadObjectError;
    OSStatus result=status(BabelDeviceAction(device,client,2));
    if(result==noErr) notify(device,kAudioDevicePropertyDeviceIsRunning);
    return result;
}
static OSStatus stop_io(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client) {
    if(!valid_driver(driver)) return kAudioHardwareBadObjectError;
    OSStatus result=status(BabelDeviceAction(device,client,3));
    if(result==noErr) notify(device,kAudioDevicePropertyDeviceIsRunning);
    return result;
}
static OSStatus zero_timestamp(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, Float64 *sample, UInt64 *host, UInt64 *seed) {
    if(!valid_driver(driver)) return kAudioHardwareBadObjectError;
    if(!sample || !host || !seed) return kAudioHardwareIllegalOperationError;
    double sample_value; uint64_t host_value, seed_value;
    OSStatus result=status(BabelZeroTimestamp(device,mach_absolute_time(),&sample_value,&host_value,&seed_value));
    if(result==noErr) { *sample=sample_value; *host=host_value; *seed=seed_value; }
    return result;
}
static OSStatus will_do(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, Boolean *will, Boolean *in_place) {
    if(!valid_driver(driver)||!valid_device(device)) return kAudioHardwareBadObjectError;
    if(!will || !in_place) return kAudioHardwareIllegalOperationError;
    *will=operation==kAudioServerPlugInIOOperationReadInput || operation==kAudioServerPlugInIOOperationWriteMix;
    *in_place=true; return noErr;
}
static OSStatus io_boundary(AudioServerPlugInDriverRef driver, AudioObjectID device, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle) {
    if(!valid_driver(driver)||!valid_device(device)) return kAudioHardwareBadObjectError;
    return frames<=4096 && cycle ? noErr : kAudioHardwareIllegalOperationError;
}
static OSStatus do_io(AudioServerPlugInDriverRef driver, AudioObjectID device, AudioObjectID stream, UInt32 client, UInt32 operation, UInt32 frames, const AudioServerPlugInIOCycleInfo *cycle, void *main_buffer, void *secondary_buffer) {
    if(!valid_driver(driver)||!valid_device(device)) return kAudioHardwareBadObjectError;
    if(!cycle) return kAudioHardwareIllegalOperationError;
    if(operation!=kAudioServerPlugInIOOperationReadInput && operation!=kAudioServerPlugInIOOperationWriteMix) return kAudioHardwareUnsupportedOperationError;
    bool input=operation==kAudioServerPlugInIOOperationReadInput;
    const AudioTimeStamp *time=input ? &cycle->mInputTime : &cycle->mOutputTime;
    if(!(time->mFlags & kAudioTimeStampSampleTimeValid)) return kAudioHardwareIllegalOperationError;
    return status(BabelProcess(device,stream,input,time->mSampleTime,frames,(float *)main_buffer));
}
static AudioServerPlugInDriverInterface interface = {
    NULL, query, retain, release, initialize, create_device, destroy_device,
    add_client, remove_client, configure, configure, has_property, is_settable,
    get_size, get_data, set_data, start_io, stop_io, zero_timestamp, will_do,
    io_boundary, do_io, io_boundary
};
void *BabelHALCreate(CFAllocatorRef allocator, CFUUIDRef requested_type) {
    if(!requested_type || !CFEqual(requested_type,kAudioServerPlugInTypeUUID)) return NULL;
    BabelRetain(); return driver_reference;
}
