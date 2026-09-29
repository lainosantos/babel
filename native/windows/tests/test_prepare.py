import importlib.util, json, os, tempfile, unittest, hashlib, zipfile
import xml.etree.ElementTree as ET
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('prepare',ROOT/'prepare.py')
prepare=importlib.util.module_from_spec(spec); spec.loader.exec_module(prepare)

class PackageTests(unittest.TestCase):
    def test_inf_registers_two_exact_pairs_without_replacing_os_generated_guid(self):
        text=(ROOT/'BabelAudio.inx').read_text()
        self.assertNotIn('\x00',text)
        for role,label in [('MicRender','Babel Microphone Feed'),('MicCapture','Babel Microphone'),('SpeakerRender','Babel Speaker'),('SpeakerCapture','Babel Speaker Monitor')]:
            self.assertIn(f'WaveBabel{role}',text)
            self.assertIn(f'TopologyBabel{role}',text)
            self.assertIn(f'%PKEY_Device_DeviceDesc%,,"{label}"',text)
        self.assertEqual(text.count('%PKEY_DeviceInterface_FriendlyName%,,"Babel Audio v1"'),4)
        self.assertEqual(text.count('%PKEY_Babel_Role%,,'),4)
        self.assertNotIn('PKEY_AudioEndpoint_GUID',text)
        self.assertIn('ROOT\\BabelAudio',text)
        self.assertIn('AddService=BabelAudio,',text)
        self.assertNotIn('SignatureAttributes.DRM',text)
    def test_inputs_have_pinned_hashes(self):
        manifest=json.loads((ROOT/'upstream.json').read_text())
        self.assertEqual(len(manifest['commit']),40)
        self.assertGreater(len(manifest['files']),30)
        for path,digest in manifest['files'].items():
            self.assertNotIn('..',Path(path).parts);self.assertEqual(len(digest),64)
    def test_installer_never_changes_boot_policy_or_uses_devcon(self):
        for name in ['install.ps1','uninstall.ps1','build.ps1','setup-wdk.ps1']:
            text=(ROOT/name).read_text().lower()
            for forbidden in ['bcdedit','set-executionpolicy','-verb runas','devcon.exe']:
                self.assertNotIn(forbidden,text)
    def test_wdk_imports_are_exact_and_escape_windows_paths(self):
        manifest=json.loads((ROOT/'wdk-packages.json').read_text())
        self.assertEqual(manifest['version'],'10.0.26100.6584')
        self.assertEqual(manifest['kit_version'],'10.0.26100.0')
        for package in manifest['packages']:
            self.assertEqual(len(package['sha256']),64)
        root=ET.fromstring(prepare.wdk_props(r'C:\Babel & CI\packages'))
        ns={'m':'http://schemas.microsoft.com/developer/msbuild/2003'}
        imports=root.findall('m:Import',ns)
        self.assertEqual(len(imports),5)
        for node in imports:
            self.assertTrue(node.attrib['Project'].startswith('C:\\Babel & CI\\packages\\'))
            self.assertIn(manifest['version'],node.attrib['Project'])
            self.assertNotIn('Exists(',node.attrib.get('Condition','')) # missing imports fail, no installed fallback
        self.assertEqual(root.find('m:PropertyGroup/m:KmdfVersion',ns).text,'1.31')
        self.assertEqual(root.find('m:PropertyGroup/m:_NT_TARGET_VERSION',ns).text,'0xA000008')
    @unittest.skipUnless(os.environ.get('BABEL_WDK_ARCHIVES'),'Set BABEL_WDK_ARCHIVES to inspected official NuGet archives')
    def test_locked_nuget_archives_contain_the_exact_imports(self):
        manifest=json.loads((ROOT/'wdk-packages.json').read_text())
        for package in manifest['packages']:
            path=Path(os.environ['BABEL_WDK_ARCHIVES'])/(package['id'].lower()+'.nupkg')
            self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(),package['sha256'])
            with zipfile.ZipFile(path) as archive:
                names={n.lower().replace('//','/') for n in archive.namelist()}
                prop='build/native/'+package['id'].lower()+'.props'
                self.assertIn(prop,names)
                if '.wdk.' in package['id'].lower():
                    self.assertIn('c/bin/10.0.26100.0/x86/inf2cat.exe',names)
                    self.assertIn('c/build/10.0.26100.0/bin/microsoft.driverkit.build.tasks.17.0.dll',names)
    @unittest.skipUnless(os.environ.get('BABEL_WDK_SOURCE') or os.environ.get('BABEL_WDK_GENERATED'),
                         'Set BABEL_WDK_SOURCE to the pinned checkout or BABEL_WDK_GENERATED to prepared output')
    def test_generated_wavert_transfers_and_cancels_before_releasing_dma(self):
        with tempfile.TemporaryDirectory() as temp:
            source=os.environ.get('BABEL_WDK_SOURCE')
            if source:
                out=Path(temp)/'wdk';prepare.prepare(out,source,r'C:\CI\packages')
                self.assertEqual((out/'Directory.Build.props').read_text(),prepare.wdk_props(r'C:\CI\packages'))
            else:
                out=Path(os.environ['BABEL_WDK_GENERATED'])
            stream=(out/'Source/Main/minwavertstream.cpp').read_text()
            self.assertIn('BabelRead(m_pMiniport->BabelCable()',stream)
            self.assertIn('BabelWrite(m_pMiniport->BabelCable()',stream)
            self.assertNotIn('GenerateSine(',stream)
            self.assertNotIn('m_SaveData.WriteData(',stream)
            self.assertLess(stream.index('ExDeleteTimer'),stream.index('m_pMiniport->Release'))
            self.assertIn('drmRights->CopyProtect || drmRights->DigitalOutputDisable',stream)
            self.assertIn('return STATUS_ACCESS_DENIED',stream)
            self.assertEqual(stream.count('BabelSpinGuard notificationGuard(&m_PositionSpinLock);'),2)
            self.assertIn('RequestedSize_ > 19200',stream)
            pairs=(out/'Source/Filters/minipairs.h').read_text()
            self.assertEqual(pairs.count('ENDPOINT_MINIPAIR '),6) # four definitions plus two callback declarations
            for name in ['BabelMicRender','BabelMicCapture','BabelSpeakerRender','BabelSpeakerCapture']:
                self.assertIn('&'+name+'Miniports,',pairs)
            # All configuration/architecture variants publish the same exact
            # package identity. Solution/resource filenames may retain upstream names.
            project=(out/'Source/Main/Main.vcxproj').read_text()
            self.assertEqual(project.count('<TargetName>BabelAudio</TargetName>'),4)
            self.assertIn('<Inf Exclude="@(Inx)" Include="*.inx" />',project)
            self.assertEqual([p.name for p in (out/'Source/Main').glob('*.inx')],['BabelAudio.inx'])
            self.assertIn('<FilesToPackage Include="$(TargetPath)"',project)
            self.assertIn('$(BabelTransportLib)',project)
            inf=(out/'Source/Main/BabelAudio.inx').read_text()
            self.assertIn('CatalogFile=BabelAudio.cat',inf)
            self.assertIn('ServiceBinary=%13%\\BabelAudio.sys',inf)
            self.assertIn('NT$ARCH$.10.0...19041',inf) # stampinf expands the WDK architecture
            self.assertNotIn('SimpleAudioSample.sys',inf)
            with self.assertRaises(ValueError):prepare.prepare(out,source)

if __name__=='__main__':unittest.main()
