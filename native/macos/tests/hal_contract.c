/* Loads our bundle into THIS test process. No HAL installation, device defaults,
 * hardware capture, root access, or coreaudiod restart is involved. */
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreFoundation/CoreFoundation.h>
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static OSStatus changed(AudioServerPlugInHostRef host, AudioObjectID object, UInt32 count, const AudioObjectPropertyAddress *addresses) { return noErr; }
static void get(AudioServerPlugInDriverRef driver, AudioObjectID object, AudioObjectPropertySelector selector, AudioObjectPropertyScope scope, UInt32 bytes, void *data) {
    AudioObjectPropertyAddress address={selector,scope,kAudioObjectPropertyElementMain}; UInt32 size=0;
    assert((*driver)->HasProperty(driver,object,getpid(),&address));
    assert((*driver)->GetPropertyDataSize(driver,object,getpid(),&address,0,NULL,&size)==noErr);
    assert(size==bytes);
    assert((*driver)->GetPropertyData(driver,object,getpid(),&address,0,NULL,bytes,&size,data)==noErr);
    assert(size==bytes);
}
int main(int argc,char **argv) {
    assert(argc==2);
    CFURLRef url=CFURLCreateFromFileSystemRepresentation(NULL,(const UInt8 *)argv[1],strlen(argv[1]),true); assert(url);
    CFPlugInRef plugin=CFPlugInCreate(NULL,url); CFRelease(url); assert(plugin);
    CFArrayRef factories=CFPlugInFindFactoriesForPlugInTypeInPlugIn(kAudioServerPlugInTypeUUID,plugin);
    assert(factories && CFArrayGetCount(factories)==1);
    CFUUIDRef factory=(CFUUIDRef)CFArrayGetValueAtIndex(factories,0);
    AudioServerPlugInDriverRef driver=(AudioServerPlugInDriverRef)CFPlugInInstanceCreate(NULL,factory,kAudioServerPlugInTypeUUID);
    assert(driver && *driver);
    AudioServerPlugInHostInterface host={0}; host.PropertiesChanged=changed;
    assert((*driver)->Initialize(driver,&host)==noErr);
    void *queried=NULL;
    assert((*driver)->QueryInterface(driver,CFUUIDGetUUIDBytes(kAudioServerPlugInDriverInterfaceUUID),&queried)==0 && queried==driver);
    (*driver)->Release(driver);
    UInt32 boxes[1],devices[2];
    get(driver,kAudioObjectPlugInObject,kAudioPlugInPropertyBoxList,kAudioObjectPropertyScopeGlobal,sizeof(boxes),boxes);
    get(driver,kAudioObjectPlugInObject,kAudioPlugInPropertyDeviceList,kAudioObjectPropertyScopeGlobal,sizeof(devices),devices);
    assert(boxes[0]==2 && devices[0]==10 && devices[1]==20);
    UInt32 acquired=0;get(driver,boxes[0],kAudioBoxPropertyAcquired,kAudioObjectPropertyScopeGlobal,sizeof(acquired),&acquired);assert(acquired==1);
    const char *uids[]={"org.babel.audio.microphone.v1","org.babel.audio.speaker.v1"};
    for(int i=0;i<2;++i) {
        CFStringRef uid=NULL; get(driver,devices[i],kAudioDevicePropertyDeviceUID,kAudioObjectPropertyScopeGlobal,sizeof(uid),&uid);
        char text[128];assert(CFStringGetCString(uid,text,sizeof(text),kCFStringEncodingUTF8));assert(strcmp(text,uids[i])==0);CFRelease(uid);
        UInt32 input,output;
        get(driver,devices[i],kAudioDevicePropertyStreams,kAudioObjectPropertyScopeInput,sizeof(input),&input);
        get(driver,devices[i],kAudioDevicePropertyStreams,kAudioObjectPropertyScopeOutput,sizeof(output),&output);
        assert(input==devices[i]+1 && output==devices[i]+2);
        AudioStreamBasicDescription format;
        get(driver,input,kAudioStreamPropertyVirtualFormat,kAudioObjectPropertyScopeGlobal,sizeof(format),&format);
        assert(format.mSampleRate==48000 && format.mChannelsPerFrame==2 && format.mBytesPerFrame==8);
        AudioObjectPropertyAddress address={kAudioStreamPropertyVirtualFormat,kAudioObjectPropertyScopeGlobal,0};
        format.mSampleRate=44100;
        assert((*driver)->SetPropertyData(driver,input,getpid(),&address,0,NULL,sizeof(format),&format)!=noErr);
        AudioServerPlugInClientInfo client={0};client.mClientID=100+i;client.mProcessID=getpid();client.mIsNativeEndian=true;
        assert((*driver)->AddDeviceClient(driver,devices[i],&client)==noErr);
        assert((*driver)->StartIO(driver,devices[i],client.mClientID)==noErr);
    }
    Float64 sample;UInt64 host_time,seed;
    assert((*driver)->GetZeroTimeStamp(driver,10,100,&sample,&host_time,&seed)==noErr);
    assert(isfinite(sample) && sample>=0 && seed>0);
    Boolean will=false,in_place=false;
    assert((*driver)->WillDoIOOperation(driver,10,100,kAudioServerPlugInIOOperationWriteMix,&will,&in_place)==noErr && will && in_place);
    float written[14],read[14];for(int i=0;i<14;i++)written[i]=(i%2?-.25f:.25f);
    AudioServerPlugInIOCycleInfo cycle={0};cycle.mOutputTime.mSampleTime=1000;cycle.mOutputTime.mFlags=kAudioTimeStampSampleTimeValid;
    cycle.mInputTime.mSampleTime=5096;cycle.mInputTime.mFlags=kAudioTimeStampSampleTimeValid;
    assert((*driver)->DoIOOperation(driver,10,12,100,kAudioServerPlugInIOOperationWriteMix,7,&cycle,written,NULL)==noErr);
    for(int reader=0;reader<2;reader++) {
        memset(read,0,sizeof(read));assert((*driver)->DoIOOperation(driver,10,11,100,kAudioServerPlugInIOOperationReadInput,7,&cycle,read,NULL)==noErr);
        assert(memcmp(read,written,sizeof(read))==0);
    }
    memset(read,1,sizeof(read));assert((*driver)->DoIOOperation(driver,20,21,101,kAudioServerPlugInIOOperationReadInput,7,&cycle,read,NULL)==noErr);
    for(int i=0;i<14;i++)assert(read[i]==0);
    assert((*driver)->StopIO(driver,10,100)==noErr);assert((*driver)->StartIO(driver,10,100)==noErr);
    assert((*driver)->DoIOOperation(driver,10,11,100,kAudioServerPlugInIOOperationReadInput,7,&cycle,read,NULL)==noErr);
    for(int i=0;i<14;i++)assert(read[i]==0);
    assert((*driver)->DoIOOperation(driver,10,11,100,kAudioServerPlugInIOOperationReadInput,4097,&cycle,read,NULL)!=noErr);
    for(int i=0;i<2;i++) {
        assert((*driver)->StopIO(driver,devices[i],100+i)==noErr);
        AudioServerPlugInClientInfo client={0};client.mClientID=100+i;
        assert((*driver)->RemoveDeviceClient(driver,devices[i],&client)==noErr);
    }
    (*driver)->Release(driver);CFRelease(factories);CFRelease(plugin);
    puts("Babel HAL SDK/CFPlugIn contract: device graph, formats, clock and loopback passed");
    return 0;
}
