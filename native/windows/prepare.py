#!/usr/bin/env python3
"""Verify pinned Microsoft WDK inputs and materialize the Babel WaveRT adapter.
No installation. Network downloads use exact commit URLs plus per-file SHA256.
"""
import argparse, hashlib, json, re, shutil, subprocess, urllib.request
from datetime import datetime
from xml.sax.saxutils import escape
from pathlib import Path
ROOT = Path(__file__).resolve().parent
MANIFEST = json.loads((ROOT / 'upstream.json').read_text())

def replace(text, old, new, count=1):
    actual = text.count(old)
    if actual != count:
        raise ValueError(f'Pinned source mismatch: expected {count}, got {actual}: {old[:80]!r}')
    return text.replace(old, new)

def driver_version(path=None):
    text=Path(path or ROOT/'BabelAudio.inx').read_text(encoding='utf-8')
    values=re.findall(r'^DriverVer=([0-9]{2}/[0-9]{2}/[0-9]{4}),([0-9]+(?:\.[0-9]+){3})\s*$',text,re.M)
    if len(values)!=1:
        raise ValueError('The driver INX must contain one explicit DriverVer date and four-part version')
    date,version=values[0]
    datetime.strptime(date,'%m/%d/%Y')
    if any(int(part)>65535 for part in version.split('.')):
        raise ValueError('Driver version components must fit in 16 bits')
    return {'date':date,'version':version}

def version_resource(text,version):
    # Keep the pinned sample's resource and notices, but never inherit the SDK's
    # Windows version as Babel's file/product version through ntverp.h.
    overrides='\n'.join(f'#undef {name}\n#define {name} {value}' for name,value in [
        ('VER_FILEVERSION',version.replace('.',',')),
        ('VER_PRODUCTVERSION',version.replace('.',',')),
        ('VER_FILEVERSION_STR',f'"{version}"'),
        ('VER_PRODUCTVERSION_STR',f'"{version}"'),
    ])
    text=replace(text,'#include <ntverp.h>','#include <ntverp.h>\n\n'+overrides)
    return text.replace('Microsoft Virtual Simple Audio Sample Driver','Babel Virtual Audio Driver').replace('SimpleAudioSample.sys','BabelAudio.sys')

def transform(base):
    release=driver_version()
    def edit(name, action):
        path = base / 'Source' / name
        path.write_text(action(path.read_text(encoding='utf-8-sig')), encoding='utf-8')
    # Two independent instances of the sample's render/capture descriptors.
    def pairs(text):
        speaker = re.search(r'static\s+ENDPOINT_MINIPAIR SpeakerMiniports =\s*\{.*?\n\};', text, re.S).group()
        mic = re.search(r'static\s+ENDPOINT_MINIPAIR MicArray1Miniports =\s*\{.*?\n\};', text, re.S).group()
        def rename(block, old, new):
            return block.replace(old+'Miniports', new+'Miniports').replace('L"Topology'+old+'"', 'L"Topology'+new+'"').replace('L"Wave'+old+'"','L"Wave'+new+'"')
        render0 = rename(speaker, 'Speaker', 'BabelMicRender')
        render1 = rename(speaker, 'Speaker', 'BabelSpeakerRender').replace('eSpeakerDevice,','eBabelSpeakerRender,')
        capture0 = rename(mic, 'MicArray1', 'BabelMicCapture')
        capture1 = rename(mic, 'MicArray1', 'BabelSpeakerCapture').replace('eMicArrayDevice1,','eBabelSpeakerCapture,')
        text = replace(text,speaker,render0+'\n\n'+render1)
        text = replace(text,mic,capture0+'\n\n'+capture1)
        text = replace(text,'    &SpeakerMiniports,','    &BabelMicRenderMiniports,\n    &BabelSpeakerRenderMiniports,')
        return replace(text,'    &MicArray1Miniports,','    &BabelMicCaptureMiniports,\n    &BabelSpeakerCaptureMiniports,')
    edit('Filters/minipairs.h', pairs)
    edit('Inc/common.h',lambda t:replace(t,'    eMaxDeviceType,','    eBabelSpeakerRender,\n    eBabelSpeakerCapture,\n    eMaxDeviceType,'))
    edit('Filters/micarraytopo.h',lambda t:replace(t,'ASSERT(m_DeviceType == eMicArrayDevice1);','ASSERT(m_DeviceType == eMicArrayDevice1 || m_DeviceType == eBabelSpeakerCapture);'))
    edit('Main/minwavert.h',lambda t:replace(replace(t,'return m_DeviceType == eSpeakerDevice ? TRUE : FALSE;','return m_DeviceType == eSpeakerDevice || m_DeviceType == eBabelSpeakerRender;'),'    BOOL IsSystemRenderPin(ULONG nPinId);','    ULONG BabelCable() const { return (m_DeviceType == eBabelSpeakerRender || m_DeviceType == eBabelSpeakerCapture) ? 1 : 0; }\n\n    BOOL IsSystemRenderPin(ULONG nPinId);'))
    edit('Main/minwavert.cpp',lambda t:replace(t,'if ((this->m_DeviceType) == eMicArrayDevice1)','if (m_DeviceType == eMicArrayDevice1 || m_DeviceType == eBabelSpeakerCapture)'))
    def capture_format(t):
        t=t.replace('32-bit','16-bit').replace('32 Bits Per Sample','16 Bits Per Sample')
        t=replace(t,'MICARRAY_32_BITS_PER_SAMPLE_PCM         32','MICARRAY_32_BITS_PER_SAMPLE_PCM         16')
        t=replace(t,'                384000,\n                8,\n                32,','                192000,\n                4,\n                16,')
        t=replace(t,'            32,\n            KSAUDIO_SPEAKER_STEREO,','            16,\n            KSAUDIO_SPEAKER_STEREO,')
        t=replace(t,'MODE_AND_DEFAULT_FORMAT MicArrayPinSupportedDeviceModes[] =\n{','MODE_AND_DEFAULT_FORMAT MicArrayPinSupportedDeviceModes[] =\n{\n    { STATIC_AUDIO_SIGNALPROCESSINGMODE_DEFAULT, &MicArrayPinSupportedDeviceFormats[0].DataFormat },')
        return t
    edit('Filters/micarraywavtable.h',capture_format)
    def stream(t):
        t=replace(t,'#include "minwavertstream.h"','#include "minwavertstream.h"\n#include "BabelTransport.h"')
        # Driver captures audio from its paired render stream, never a synthetic tone.
        start=t.index('    if (m_bCapture)\n    {\n        ReadRegistrySettings();')
        end=t.index('    //\n    // Register this stream.',start)
        t=t[:start]+'''    // All four hardware endpoints have one exact integer format.
    if (m_pWfExt->Format.nSamplesPerSec != 48000 ||
        m_pWfExt->Format.nChannels != 2 || m_pWfExt->Format.wBitsPerSample != 16 ||
        m_pWfExt->Format.nBlockAlign != 4) return STATUS_NOT_SUPPORTED;

'''+t[end:]
        t=replace(t,'m_ToneGenerator.GenerateSine(m_pDmaBuffer + bufferOffset, runWrite);','BabelRead(m_pMiniport->BabelCable(), m_pDmaBuffer + bufferOffset, runWrite);')
        t=replace(t,'m_SaveData.WriteData(m_pDmaBuffer + bufferOffset, runWrite);','BabelWrite(m_pMiniport->BabelCable(), m_pDmaBuffer + bufferOffset, runWrite);')
        t=replace(t,'''        if (!g_DoNotCreateDataFiles)
        {
            // Read from buffer and write to a file.
            ReadBytes(ByteDisplacement);
        }''','''        // Transfer render frames into this cable's Rust transport.
        ReadBytes(ByteDisplacement);''')
        # Bound late timer catch-up to one DMA cycle, preserving the latest frames.
        t=replace(t,'    ULONG bufferOffset = m_ullLinearPosition % m_ulDmaBufferSize;', '''    if (!m_pDmaBuffer || !m_ulDmaBufferSize) return;
    ULONG skip = ByteDisplacement > m_ulDmaBufferSize ? ByteDisplacement - m_ulDmaBufferSize : 0;
    ULONG bufferOffset = (m_ullLinearPosition + skip) % m_ulDmaBufferSize;
    ByteDisplacement = min(ByteDisplacement, m_ulDmaBufferSize);''',count=2)
        # Pause resets the cable before any final position update; RUN starts fresh.
        t=replace(t,'    // Spew an event for a pin state change request from portcls','    BabelSetRunning(m_pMiniport->BabelCable(), m_bCapture, State_ == KSSTATE_RUN && !InterlockedCompareExchange(&m_BabelProtected, 0, 0));\n\n    // Spew an event for a pin state change request from portcls')
        t=replace(t,'    m_ulContentId = contentId;', '''    if (drmRights->CopyProtect || drmRights->DigitalOutputDisable) {
        InterlockedExchange(&m_BabelProtected, 1);
        BabelSetRunning(m_pMiniport->BabelCable(), m_bCapture, FALSE);
        return STATUS_ACCESS_DENIED;
    }
    InterlockedExchange(&m_BabelProtected, 0);
    m_ulContentId = contentId;''')
        # Serialize notification membership against the DPC before freeing nodes.
        # These two methods must be nonpaged while the spin guard raises IRQL.
        for method in ['RegisterNotificationEvent','UnregisterNotificationEvent']:
            marker='NTSTATUS CMiniportWaveRTStream::'+method
            t=replace(t,'#pragma code_seg("PAGE")\n'+marker,'#pragma code_seg()\n'+marker)
            start=t.index(marker)
            end=t.index('//=============================================================================',start)
            block=t[start:end]
            block=replace(block,'    PAGED_CODE();','    BabelSpinGuard notificationGuard(&m_PositionSpinLock);')
            t=t[:start]+block+t[end:]
        t=replace(t,'    // Convert ticks to 100ns units.\n    LONGLONG  hnsCurrentTime = KSCONVERT_PERFORMANCE_TIME(m_ullPerformanceCounterFrequency.QuadPart, ilQPC);',
            '    if (!m_pDmaBuffer || !m_ulDmaBufferSize || !m_ullPerformanceCounterFrequency.QuadPart) return;\n    // Convert ticks to 100ns units.\n    LONGLONG  hnsCurrentTime = KSCONVERT_PERFORMANCE_TIME(m_ullPerformanceCounterFrequency.QuadPart, ilQPC);')
        t=replace(t,'        case KSSTATE_STOP:\n', '        case KSSTATE_STOP:\n            if (m_pNotificationTimer) { ExCancelTimer(m_pNotificationTimer, NULL); KeFlushQueuedDpcs(); }\n')
        # Limit one DMA buffer to 100 ms. Larger requests would turn one late
        # callback into unbounded work while holding the position spin lock.
        t=replace(t,'    ULONG ulBufferDurationMs = 0;', '    ULONG ulBufferDurationMs = 0;\n    if (RequestedSize_ > 19200) return STATUS_INVALID_PARAMETER;')
        t=replace(t,'if ((NotificationCount_ == 0) || (RequestedSize_ % NotificationCount_ != 0))',
            'if ((NotificationCount_ == 0) || (RequestedSize_ % NotificationCount_ != 0) || (RequestedSize_ / NotificationCount_ < 192))')
        t=replace(t,'    if ((0 == RequestedSize_) || (RequestedSize_ < m_pWfExt->Format.nBlockAlign))',
            '    if ((0 == RequestedSize_) || RequestedSize_ > 19200 || (RequestedSize_ < m_pWfExt->Format.nBlockAlign))')
        # Cancel timers before releasing miniport/DMA storage (the upstream order
        # releases storage first). This matters for actual shared audio transport.
        timer_start=t.index('    if (m_pNotificationTimer)\n')
        timer_end=t.index('    DPF_ENTER(("[CMiniportWaveRTStream::~CMiniportWaveRTStream]"));',timer_start)
        timer=t[timer_start:timer_end]
        t=t[:timer_start]+t[timer_end:]
        t=replace(t,'    PAGED_CODE();\n    if (NULL != m_pMiniport)','    PAGED_CODE();\n'+timer+'    if (m_pMiniport && m_bUnregisterStream) BabelSetRunning(m_pMiniport->BabelCable(), m_bCapture, FALSE);\n    if (NULL != m_pMiniport)')
        return t
    edit('Main/minwavertstream.cpp',stream)
    edit('Main/minwavertstream.h',lambda t:replace(t,'    ToneGenerator               m_ToneGenerator;','    volatile LONG m_BabelProtected = 0; // Protected audio never enters the cable.'))
    def adapter(t):
        t=replace(t,'#include "minipairs.h"','#include "minipairs.h"\n#include "BabelTransport.h"')
        t=replace(t,'    DPF(D_TERSE, ("[DriverEntry]"));','    DPF(D_TERSE, ("[DriverEntry]"));\n    BabelInitializeTransport();')
        # No registry preference can re-enable the sample's audio file recording.
        t=re.sub(r'^.*RTL_QUERY_REGISTRY_DIRECT.*L"DoNotCreateDataFiles".*\n','',t,flags=re.M)
        return t
    edit('Main/adapter.cpp',adapter)
    # A failed adapter allocation must release the sample's singleton claim.
    edit('Main/common.cpp',lambda t:replace(t,'        DPF(D_ERROR, ("NewAdapterCommon failed, 0x%x", ntStatus));','        InterlockedExchange(&CAdapterCommon::m_AdapterInstances, 0);\n        DPF(D_ERROR, ("NewAdapterCommon failed, 0x%x", ntStatus));'))
    for name in ['BabelTransport.h','BabelTransport.cpp']:
        shutil.copy2(ROOT/'shim'/name,base/'Source/Main'/name)
    shutil.copy2(ROOT/'BabelAudio.inx',base/'Source/Main/BabelAudio.inx')
    (base/'Source/Main/SimpleAudioSample.inx').unlink()
    stamp=f'<Inf><SpecifyDriverVerDirectiveDate>true</SpecifyDriverVerDirectiveDate><DateStamp>{release["date"]}</DateStamp><SpecifyDriverVerDirectiveVersion>true</SpecifyDriverVerDirectiveVersion><TimeStamp>{release["version"]}</TimeStamp></Inf>'
    edit('Main/Main.vcxproj',lambda t:replace(t,'    <ClCompile Include="adapter.cpp" />','    <ClCompile Include="BabelTransport.cpp" />\n    <ClCompile Include="adapter.cpp" />').replace('<TargetName>SimpleAudioSample</TargetName>','<TargetName>BabelAudio</TargetName>').replace('  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.targets" />',f'  <ItemDefinitionGroup>{stamp}</ItemDefinitionGroup>\n'+'''  <ItemDefinitionGroup><Link><AdditionalDependencies>%(AdditionalDependencies);$(BabelTransportLib)</AdditionalDependencies></Link></ItemDefinitionGroup>
  <Target Name="RequireBabelTransport" BeforeTargets="Link"><Error Condition="!Exists('$(BabelTransportLib)')" Text="Build the Babel Rust transport with build.ps1 first." /></Target>
  <Import Project="$(VCTargetsPath)\\Microsoft.Cpp.targets" />'''))
    edit('Main/SimpleAudioSample.rc',lambda t:version_resource(t,release['version']))
    (base/'driver-version.json').write_text(json.dumps(release)+'\n',encoding='utf-8')
    (base/'UPSTREAM.txt').write_text(MANIFEST['repository']+'\n'+MANIFEST['commit']+'\nMicrosoft source and modifications: MS-PL. See LICENSE-Microsoft.txt.\n')
    shutil.copy2(ROOT/'LICENSE-Microsoft.txt',base/'LICENSE-Microsoft.txt')

def wdk_props(packages):
    """Use the same import order as the official sample's Directory.Build.props.

    Both NuGet archives and source inputs are independently hash-pinned. The
    VS2022 WDK extension still supplies the platform-toolset entry point.
    """
    manifest=json.loads((ROOT/'wdk-packages.json').read_text())
    version=manifest['version']
    root=escape(str(packages).replace('/','\\').rstrip('\\'),{'"':'&quot;'})
    lines=['<Project xmlns="http://schemas.microsoft.com/developer/msbuild/2003">']
    for name,platform,suffix in [('Microsoft.Windows.WDK','x64','x64'),('Microsoft.Windows.WDK','ARM64','arm64'),
                                 ('Microsoft.Windows.SDK.CPP','x64','x64'),('Microsoft.Windows.SDK.CPP','ARM64','arm64')]:
        prop=name.replace('CPP','cpp')+'.'+suffix+'.props'
        lines.append(f'  <Import Project="{root}\\{name}.{suffix}.{version}\\build\\native\\{prop}" Condition="\'$(Platform)\' == \'{platform}\'" />')
    lines.append(f'  <Import Project="{root}\\Microsoft.Windows.SDK.CPP.{version}\\build\\native\\Microsoft.Windows.SDK.cpp.props" />')
    # Windows 10 2004 is the declared minimum in the INF; do not accidentally
    # link against a newer OS or KMDF just because a newer SDK was selected.
    lines.append('  <PropertyGroup><KmdfVersion>1.31</KmdfVersion><_NT_TARGET_VERSION>0xA000008</_NT_TARGET_VERSION><TargetVersion>Windows10</TargetVersion></PropertyGroup>')
    lines.append('</Project>')
    return '\n'.join(lines)+'\n'

def prepare(destination, source_root=None, wdk_packages=None):
    destination=Path(destination)
    if destination.exists():
        raise ValueError('Output must be a new directory; refusing to overwrite an existing build.')
    destination.mkdir(parents=True)
    for relative,digest in MANIFEST['files'].items():
        if source_root:
            source_root=Path(source_root)
            if (source_root/'.git').exists():
                # The sample's .gitattributes changes CRLF and converts .inx to
                # UTF-16 on checkout. Hash the original blob, exactly as served
                # by the pinned raw URL, rather than machine-dependent worktree bytes.
                data=subprocess.run(['git','-C',str(source_root),'cat-file','blob',
                                     f"{MANIFEST['commit']}:{relative}"],
                                    check=True,stdout=subprocess.PIPE).stdout
            else:
                data=(source_root/relative).read_bytes()
        else:
            url=f"https://raw.githubusercontent.com/microsoft/Windows-driver-samples/{MANIFEST['commit']}/{relative}"
            with urllib.request.urlopen(url,timeout=60) as response: data=response.read()
        if hashlib.sha256(data).hexdigest()!=digest: raise ValueError('SHA256 mismatch: '+relative)
        prefix='audio/simpleaudiosample/'
        if relative.startswith(prefix):
            path=destination/relative[len(prefix):]
            path.parent.mkdir(parents=True,exist_ok=True)
            path.write_bytes(data)
    transform(destination)
    if wdk_packages:
        (destination/'Directory.Build.props').write_text(wdk_props(wdk_packages),encoding='utf-8')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,default=ROOT/'build'/'wdk')
    parser.add_argument('--source-root',type=Path,help='Offline Git repository containing the pinned commit (reads original blobs, not working-tree files), or an export of those exact bytes; hashes are checked')
    parser.add_argument('--wdk-packages',help='Verified NuGet directory from setup-wdk.ps1; imports the pinned SDK/WDK')
    args=parser.parse_args();prepare(args.output,args.source_root,args.wdk_packages)
    print('Prepared Babel WaveRT sources:',args.output)
