// Host-only property-handler contract test; no Windows endpoint is opened.
#include "definitions.h"
#include "BabelVolume.h"
#include <cassert>
#define _In_
#define PAGED_CODE()
#define DPF_ENTER(x)
#define DPF(...)
#define TRUE 1
#define NT_SUCCESS(value) ((value)>=0)
#define VOLUME_NORMALIZE_IN_RANGE(value) (value)
using NTSTATUS=LONG; using PLONG=LONG*; using PULONG=ULONG*; using PBOOL=BOOL*;
constexpr LONG STATUS_SUCCESS=0, STATUS_INVALID_DEVICE_REQUEST=-1, STATUS_INVALID_PARAMETER=-2;
constexpr ULONG ALL_CHANNELS_ID=UINT32_MAX;
constexpr ULONG KSPROPERTY_TYPE_BASICSUPPORT=1,KSPROPERTY_TYPE_GET=2,KSPROPERTY_TYPE_SET=4;
struct Request { ULONG Verb,Node,ValueSize,InstanceSize; void* Value; void* Instance; };
using PPCPROPERTY_REQUEST=Request*;
NTSTATUS ValidatePropertyParams(Request* request,ULONG value,ULONG instance) {
    return request->Value && request->Instance && request->ValueSize>=value && request->InstanceSize>=instance ? 0 : -2;
}
NTSTATUS PropertyHandler_BasicSupportVolume(Request*,ULONG) { return 0; }
NTSTATUS PropertyHandler_BasicSupportMute(Request*,ULONG) { return 0; }
#include "volume_handlers.inc"

int main() {
    BabelVolumeState first, second;
    ULONG channel=ALL_CHANNELS_ID;
    LONG level=-12*65536;
    Request request{KSPROPERTY_TYPE_SET,0,sizeof(level),sizeof(channel),&level,&channel};
    assert(PropertyHandler_Volume(&first,&request,2)==STATUS_SUCCESS);
    assert(first.MixerVolumeRead(0,0)==level && first.MixerVolumeRead(0,1)==level);
    assert(second.MixerVolumeRead(0,0)==0 && second.MixerVolumeRead(0,1)==0);
    channel=1;level=-24*65536;
    assert(PropertyHandler_Volume(&first,&request,2)==STATUS_SUCCESS);
    assert(first.MixerVolumeRead(0,0)==-12*65536 && first.MixerVolumeRead(0,1)==level);
    channel=2;
    assert(PropertyHandler_Volume(&first,&request,2)==STATUS_INVALID_PARAMETER);
    channel=ALL_CHANNELS_ID;level=1;request.Node=1;
    assert(PropertyHandler_Mute(&first,&request,2)==STATUS_SUCCESS);
    assert(first.MixerMuteRead(1,0) && first.MixerMuteRead(1,1));
    assert(!second.MixerMuteRead(1,0) && !second.MixerMuteRead(1,1));
    request.Verb=KSPROPERTY_TYPE_GET;channel=1;level=0;
    assert(PropertyHandler_Mute(&first,&request,2)==STATUS_SUCCESS && level==1);
}
