"""Focused producer/package evidence faults; fixtures stay under .artifacts."""
from pathlib import Path
import importlib.util,json,os,subprocess,sys,time,unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('provenance',ROOT/'scripts/build_record.py')
provenance=importlib.util.module_from_spec(spec);spec.loader.exec_module(provenance)
RUN=ROOT/'.artifacts/build-record-tests'/str(time.time_ns())

def put(path,text='fixture'):
    path.parent.mkdir(parents=True,exist_ok=True);path.write_text(text);return path

class Evidence(unittest.TestCase):
    def setUp(self):
        self.case=RUN/self._testMethodName;self.root=self.case/'checkout';self.env={'PATH':os.environ['PATH'],'MACOSX_DEPLOYMENT_TARGET':'15.0'}
        self.root.mkdir(parents=True)
        for name in (*provenance.ROOT_INPUTS,*provenance.SCRIPT_INPUTS):put(self.root/name)
        for folder in ('crates/core','bindings/node','providers/typescript','proto'):put(self.root/folder/'input.rs')
        put(self.root/'crates/core/src/lib.rs','pub fn fixture() {}')
        for name in provenance.PACKAGED_DOCS:put(self.root/'docs'/name)
        for name in ('scripts/package.py','LICENSE','bindings/node/README.md','bindings/dotnet/KnowCode/README.md','third_party/licenses/index.json'):put(self.root/name)
        put(self.root/'providers/typescript/package.json',json.dumps({'dependencies':{'mini':'1.0.0'}}))
        self.nested=self.root/'node_modules/mini/node_modules/nested'
        put(self.root/'node_modules/mini/package.json',json.dumps({'name':'mini','version':'1.0.0','dependencies':{'nested':'2.0.0'}}))
        put(self.nested/'package.json',json.dumps({'name':'nested','version':'2.0.0'}));put(self.nested/'settings.yaml','source: pinned\n')
        put(self.root/'package-lock.json',json.dumps({'packages':{'node_modules/mini':{'version':'1.0.0'},'node_modules/mini/node_modules/nested':{'version':'2.0.0'}}}))
        crate=self.case/'cargo-fixture';put(crate/'Cargo.toml','fixture crate');put(crate/'src/lib.rs','fixture dependency')
        self.metadata={'packages':[{'id':'registry#fixture@1.0.0','name':'fixture','version':'1.0.0','source':'registry','manifest_path':str(crate/'Cargo.toml')}],'workspace_members':['registry#fixture@1.0.0'],'resolve':{'nodes':[{'id':'registry#fixture@1.0.0','deps':[]}]}}
        self.nuget=self.case/'nuget/fixture/1.0.0';put(self.nuget/'lib/Dependency.dll','dependency')
        for project,assembly in zip(provenance.PROJECTS,('KnowCode.Provider.DotNet','KnowCode','Smoke')):
            put(self.root/project/'source.cs')
            assets={'packageFolders':{str(self.case/'nuget'):{}},'libraries':{'Fixture/1.0.0':{'type':'package','path':'fixture/1.0.0'}}}
            put(self.root/project/'obj/project.assets.json',json.dumps(assets))
            for suffix in ('.dll','.deps.json'):put(self.root/project/f'bin/Debug/net10.0/{assembly}{suffix}')
            put(self.root/project/'obj/Debug/net10.0/Generated.cs')
            put(self.root/project/'obj/Debug/net10.0'/f'{assembly}.dll')
        self.native=[put(self.root/'target/debug'/name,name) for name in ('know-code','libknow_code_node.dylib','libknow_code_ffi.dylib')]
        put(self.root/'.artifacts/build/debug/swift/indexstore-reader')
        put(self.root/'.artifacts/build/debug/node/index.mjs')
        put(self.root/'.artifacts/build/debug/node/know_code.node','libknow_code_node.dylib')
        put(self.root/'bindings/dotnet/Smoke/bin/Debug/net10.0/libknow_code_ffi.dylib','libknow_code_ffi.dylib')
        put(self.root/'providers/dotnet/obj/protocol/Analysis.cs')
        out=self.root/'target/debug/build/know-code-core-hash/out';put(out/'knowcode.rs')
        events=[{'reason':'build-script-executed','package_id':'path#know-code-core@0.0.1','out_dir':str(out)},{'reason':'compiler-artifact','filenames':list(map(str,self.native))},{'reason':'build-finished','success':True}]
        put(self.root/'.artifacts/build/debug/cargo-build.jsonl',''.join(json.dumps(e)+'\n' for e in events))
        self.tools=patch.object(provenance,'toolchains',lambda *_:{'compiler':{'identity':'fixture-v1'}});self.tools.start();self.addCleanup(self.tools.stop)
        self.commands=patch.object(provenance,'run',lambda *_:json.dumps(self.metadata));self.commands.start();self.addCleanup(self.commands.stop)
    def complete(self):
        record=provenance.begin(self.root,'debug',self.env);provenance.prepare(self.root,'debug',self.env,record)
        return provenance.finish(self.root,'debug',self.env,record)
    def verify(self):return provenance.verify(self.root,'debug',self.env)
    def test_complete_record_covers_restored_generated_and_all_outputs(self):
        expected=self.complete();self.assertEqual(self.verify(),expected)
        self.assertIn('providers/dotnet/obj/protocol/Analysis.cs',expected['generated_inputs'])
        self.assertIn('node_modules/mini/node_modules/nested/settings.yaml',expected['dependency_inputs'])
        self.assertIn('bindings/dotnet/KnowCode/bin/Debug/net10.0/KnowCode.dll',expected['outputs'])
    def test_missing_partial_and_wrong_profile_records_are_rejected(self):
        with self.assertRaisesRegex(provenance.Invalid,'Run bash scripts/build.sh debug'):self.verify()
        self.complete();path=provenance.record_path(self.root,'debug');record=json.loads(path.read_text());record['profile']='release';provenance.write(path,record)
        with self.assertRaisesRegex(provenance.Invalid,'different profile'):self.verify()
        record['profile']='debug';record['status']='building';provenance.write(path,record)
        with self.assertRaisesRegex(provenance.Invalid,'incomplete'):self.verify()
        record['status']='complete';record.pop('build_flags');provenance.write(path,record)
        with self.assertRaisesRegex(provenance.Invalid,'producer metadata'):self.verify()
        provenance.write(path,[])
        with self.assertRaisesRegex(provenance.Invalid,'incomplete'):self.verify()
    def test_build_cannot_certify_changed_inputs_or_dependency_bytes(self):
        record=provenance.begin(self.root,'debug',self.env);provenance.prepare(self.root,'debug',self.env,record)
        put(self.root/'crates/core/src/lib.rs','changed during build')
        with self.assertRaisesRegex(provenance.Invalid,'during compilation'):provenance.finish(self.root,'debug',self.env,record)
        self.assertFalse(provenance.record_path(self.root,'debug').exists())
        self.assertTrue(provenance.pending_path(self.root,'debug').exists())
    def test_restoration_cannot_change_frozen_source(self):
        record=provenance.begin(self.root,'debug',self.env);put(self.root/'package-lock.json','changed during restoration')
        with self.assertRaisesRegex(provenance.Invalid,'during restoration'):provenance.prepare(self.root,'debug',self.env,record)
    def test_changed_input_same_revision_is_rejected(self):
        self.complete();put(self.root/'crates/core/src/lib.rs','changed without commit')
        with self.assertRaisesRegex(provenance.Invalid,'build inputs changed'):self.verify()
    def test_package_only_document_changes_do_not_require_rebuild(self):
        self.complete();before=provenance.package_inputs(self.root)
        for name in ('docs/MCP.md','bindings/node/README.md','scripts/package.py'):put(self.root/name,'new package-only content')
        self.verify();self.assertNotEqual(provenance.package_inputs(self.root),before)
    def test_changed_nested_dependency_and_generated_schema_are_rejected(self):
        self.complete();put(self.nested/'settings.yaml','source: tampered\n')
        with self.assertRaisesRegex(provenance.Invalid,'dependency inputs changed'):self.verify()
        put(self.nested/'settings.yaml','source: pinned\n');put(self.root/'providers/dotnet/obj/protocol/Analysis.cs','wrong protocol')
        with self.assertRaisesRegex(provenance.Invalid,'generated inputs changed'):self.verify()
    def test_restoration_cannot_change_external_or_new_root_configuration(self):
        self.env['CARGO_HOME']=str(self.case/'cargo-home');config=put(self.case/'cargo-home/config.toml','[build]\n')
        record=provenance.begin(self.root,'debug',self.env);put(config,'[build]\nrustflags=[]\n')
        with self.assertRaisesRegex(provenance.Invalid,'configuration inputs during restoration'):provenance.prepare(self.root,'debug',self.env,record)
        put(config,'[build]\n');put(self.root/'Directory.Packages.props','new restore inputs')
        with self.assertRaisesRegex(provenance.Invalid,'configuration inputs during restoration'):provenance.prepare(self.root,'debug',self.env,record)
    def test_local_path_dependency_outside_the_workspace_is_frozen(self):
        local=self.case/'local-patch';put(local/'Cargo.toml');code=put(local/'src/lib.rs','original patch')
        identity='path#local-patch@1.0.0'
        self.metadata['packages'].append({'id':identity,'version':'1.0.0','source':None,'manifest_path':str(local/'Cargo.toml')})
        self.metadata['resolve']['nodes'][0]['deps'].append({'pkg':identity,'dep_kinds':[{'kind':None}]})
        self.metadata['resolve']['nodes'].append({'id':identity,'deps':[]})
        self.complete();put(code,'changed patch without a manifest/lock edit')
        with self.assertRaisesRegex(provenance.Invalid,'dependency inputs changed'):self.verify()
    def test_changed_native_and_provider_payloads_are_rejected(self):
        self.complete();dll=self.root/'providers/dotnet/bin/Debug/net10.0/KnowCode.Provider.DotNet.dll';put(dll,'stale worker')
        with self.assertRaisesRegex(provenance.Invalid,'built outputs changed'):self.verify()
        put(dll);put(self.root/'.artifacts/build/debug/node/know_code.node','wrong native')
        with self.assertRaisesRegex(provenance.Invalid,'mixed outputs'):self.verify()
    def test_changed_restored_nuget_payload_is_rejected(self):
        self.complete();put(self.nuget/'lib/Dependency.dll','tampered runtime dependency')
        with self.assertRaisesRegex(provenance.Invalid,'dependency inputs changed'):self.verify()
    def test_changed_external_build_configuration_is_rejected(self):
        self.env['CARGO_HOME']=str(self.case/'cargo-home');config=put(self.case/'cargo-home/config.toml','[build]\n')
        self.complete();put(config,'[build]\nrustflags=["-C", "opt-level=3"]\n')
        with self.assertRaisesRegex(provenance.Invalid,'configuration inputs changed'):self.verify()
    def test_finished_build_rejects_changed_dependency_or_flags(self):
        record=provenance.begin(self.root,'debug',self.env);provenance.prepare(self.root,'debug',self.env,record)
        dependency=self.nuget/'lib/Dependency.dll';put(dependency,'changed during compile')
        with self.assertRaisesRegex(provenance.Invalid,'dependency inputs during compilation'):provenance.finish(self.root,'debug',self.env,record)
        put(dependency,'dependency');changed=dict(self.env,RUSTFLAGS='-C opt-level=3')
        with self.assertRaisesRegex(provenance.Invalid,'flags during compilation'):provenance.finish(self.root,'debug',changed,record)
    def test_packaged_payload_is_bound_to_the_producer_bytes(self):
        record=self.complete();name='target/debug/libknow_code_ffi.dylib'
        provenance.require_output(record,name,provenance.digest(self.root/name),'debug')
        with self.assertRaisesRegex(provenance.Invalid,'packaged payload does not match'):provenance.require_output(record,name,'0'*64,'debug')
        with self.assertRaisesRegex(provenance.Invalid,'packaged payload does not match'):provenance.require_output(record,'unbuilt.dll','0'*64,'debug')
    def test_later_build_attempt_blocks_old_complete_record(self):
        self.complete();provenance.begin(self.root,'debug',self.env)
        with self.assertRaisesRegex(provenance.Invalid,'unfinished build'):self.verify()
    def test_incomplete_cargo_evidence_cannot_certify_old_native_files(self):
        record=provenance.begin(self.root,'debug',self.env);provenance.prepare(self.root,'debug',self.env,record)
        log=self.root/'.artifacts/build/debug/cargo-build.jsonl';events=[json.loads(line) for line in log.read_text().splitlines()]
        events=[e for e in events if e['reason']!='compiler-artifact'];put(log,'\n'.join(map(json.dumps,events)))
        with self.assertRaisesRegex(provenance.Invalid,'every required native output'):provenance.finish(self.root,'debug',self.env,record)
    def test_producer_and_packaging_cannot_overlap(self):
        with provenance.lock(self.root):
            with self.assertRaises(provenance.Invalid):provenance.lock(self.root,shared=True)
        with provenance.lock(self.root,shared=True),provenance.lock(self.root,shared=True):
            with self.assertRaises(provenance.Invalid):provenance.lock(self.root)
    def test_writer_child_retains_fence_after_driver_exit(self):
        writer="import os,sys; print('ready',flush=True); sys.stdin.buffer.read(1); os.close(int(sys.argv[1])); print('released',flush=True)"
        driver=f"import os,sys; from pathlib import Path; sys.path.insert(0,{str(ROOT/'scripts')!r}); import build_record as p\nwith p.lock(Path({str(self.root)!r})) as fence: p.run([sys.executable,'-c',{writer!r},str(fence.fileno())],Path({str(self.root)!r}),dict(os.environ),False,(fence.fileno(),))"
        process=subprocess.Popen([sys.executable,'-c',driver],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
        try:
            self.assertEqual(process.stdout.readline().strip(),'ready')
            process.terminate();process.wait(timeout=5)
            with self.assertRaises(provenance.Invalid):provenance.lock(self.root)
            process.stdin.write('x');process.stdin.flush()
            self.assertEqual(process.stdout.readline().strip(),'released')
            with provenance.lock(self.root):pass
        finally:
            process.stdin.close();process.stdout.close()
            if process.poll() is None:process.kill()
            process.wait(timeout=5)

if __name__=='__main__':
    result=unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Evidence))
    RUN.mkdir(parents=True,exist_ok=True)
    (RUN/'result.json').write_text(json.dumps({'passed':result.wasSuccessful(),'cases':result.testsRun,'run':str(RUN)},indent=2)+'\n')
    print(json.dumps({'passed':result.wasSuccessful(),'cases':result.testsRun,'run':str(RUN)}))
    raise SystemExit(not result.wasSuccessful())
