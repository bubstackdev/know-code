#!/usr/bin/env python3
"""Assemble local candidate artifacts from matching built outputs; never publish."""
from pathlib import Path
import argparse,gzip,hashlib,json,os,platform,shutil,subprocess,tarfile,time,tomllib,xml.etree.ElementTree as ET,zipfile
import build_record as provenance
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--profile',choices=['debug','release'],default='release');parser.add_argument('--out',type=Path)
args=parser.parse_args();PROFILE=args.profile;CONFIG=PROFILE.capitalize();VERSION=tomllib.loads((ROOT/'Cargo.toml').read_text())['workspace']['package']['version']
try:
 BUILD_LOCK=provenance.lock(ROOT)
 BUILD_RECORD=provenance.verify(ROOT,PROFILE)
 PACKAGE_INPUTS=provenance.package_inputs(ROOT)
except provenance.Invalid as error:raise SystemExit(str(error))
OUT=(args.out or ROOT/'.artifacts/packages'/PROFILE/str(time.time_ns())).resolve();OUT.mkdir(parents=True,exist_ok=True)
if platform.system()!='Darwin' or platform.machine()!='arm64':raise SystemExit('Build these candidate artifacts on macOS Apple Silicon.')
STAGE=OUT/'staging';STAGE.mkdir();RUNTIME=STAGE/f'know-code-{VERSION}-darwin-arm64';RESOURCES=RUNTIME/'lib/know-code';RESOURCES.mkdir(parents=True);(RUNTIME/'bin').mkdir()
ENV=dict(os.environ,DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',npm_config_cache=str(OUT/'.npm-cache'),npm_config_audit='false',npm_config_fund='false')
def run(*command,cwd=ROOT):
 return provenance.run(list(map(str,command)),cwd,ENV,inherited=(BUILD_LOCK.fileno(),),timeout=180)

def copy(source,dest):
 if source.is_symlink():raise RuntimeError(f'Unexpected source symlink: {source}')
 dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(source,dest)

def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()

# Build-time notice evidence and dependency inventory, retained alongside each artifact.
NOTICES=STAGE/'THIRD_PARTY';NOTICES.mkdir();inventory=[]
def pinned_notice(name,version,license_text,commit):
 key=name+'@'+version
 pinned=json.loads((ROOT/'third_party/licenses/index.json').read_text()).get(key)
 if not pinned or pinned['commit']!=commit or pinned['license']!=license_text:
  raise provenance.Invalid(f'Missing matching upstream license evidence for {key}; pin the exact restored repository revision and license text before packaging.')
 supplied=ROOT/pinned['file']
 if supplied.is_symlink() or not supplied.is_file() or not supplied.resolve().is_relative_to(ROOT/'third_party/licenses') or digest(supplied)!=pinned['sha256']:
  raise provenance.Invalid(f'Invalid pinned license content for {key}.')
 return supplied,pinned
def evidence(kind,name,version,license_text,source,files):
 entry={'ecosystem':kind,'name':name,'version':version,'license':license_text,'source':source,'notice_files':[]}
 for file in files:
  if not file.is_file():continue
  suffix=file.suffix if len(file.suffix)<12 else '.txt';dest=NOTICES/'texts'/(digest(file)+suffix)
  if not dest.exists():copy(file,dest)
  entry['notice_files'].append(str(dest.relative_to(NOTICES)))
 inventory.append(entry)

# Runtime JS dependencies come from the installed pinned closure, never workspace links.
for source in provenance.runtime_packages(ROOT):
 key=source.relative_to(ROOT).as_posix()
 manifest=json.loads((source/'package.json').read_text());name=manifest['name']
 # Preserve installed nesting so conflicting versions keep Node's resolution.
 # Recurse declared dependencies explicitly rather than copying incidental/dev trees.
 shutil.copytree(source,RESOURCES/key,symlinks=False,ignore=shutil.ignore_patterns('node_modules'))
 evidence('npm',name,manifest['version'],manifest.get('license',''),f'https://registry.npmjs.org/{name}',[p for p in source.rglob('*') if p.is_file() and any(t in p.name.lower() for t in ['license','notice','copyright'])])
for provider in ['typescript','shared','rust','swift']:
 for source in (ROOT/'providers'/provider).glob('*.mjs'):copy(source,RESOURCES/'providers'/provider/source.name)
copy(ROOT/'proto/analysis.proto',RESOURCES/'proto/analysis.proto')
copy(ROOT/f'.artifacts/build/{PROFILE}/swift/indexstore-reader',RESOURCES/'providers/swift/indexstore-reader')

# Framework-dependent publication keeps SDK/runtime installation external.
run('dotnet','publish',ROOT/'providers/dotnet/KnowCode.Provider.DotNet.csproj','--no-restore','--no-build','--self-contained','false','-p:UseAppHost=false','-c',CONFIG,'-o',RESOURCES/'providers/dotnet')
for pdb in (RESOURCES/'providers/dotnet').rglob('*.pdb'):pdb.unlink()
for payload in provenance.files(RESOURCES/'providers/dotnet'):
 provenance.require_output(BUILD_RECORD,f'providers/dotnet/bin/{CONFIG}/net10.0/{payload.relative_to(RESOURCES/"providers/dotnet").as_posix()}',digest(payload),PROFILE)
assets=json.loads((ROOT/'providers/dotnet/obj/project.assets.json').read_text())
for name,library in assets['libraries'].items():
 if library.get('type')!='package':continue
 package,version=name.rsplit('/',1);directory=next(Path(folder)/library['path'] for folder in assets['packageFolders'] if (Path(folder)/library['path']).is_dir());nuspec=next(directory.glob('*.nuspec'))
 tree=ET.parse(nuspec);metadata=next(e for e in tree.getroot() if e.tag.endswith('metadata'))
 lic=next((e for e in metadata if e.tag.endswith('license')),None);repository=next((e for e in metadata if e.tag.endswith('repository')),None)
 # A nuspec SPDX expression/URL is metadata, not the redistributed upstream
 # license text. Preserve both; fail closed if no matching text is supplied.
 texts=[p for p in directory.rglob('*') if p.is_file() and p.suffix not in ('.nupkg','.sha512','.nuspec') and any(t in p.name.lower() for t in ['license','notice','copyright'])]
 pinned=None
 if not texts:
  supplied,pinned=pinned_notice(package,version,lic.text if lic is not None else '',repository.attrib.get('commit') if repository is not None else None)
  texts=[supplied]
 files=[nuspec,*texts]
 evidence('nuget',package,version,lic.text if lic is not None else '',repository.attrib if repository is not None else {},files)
 if pinned:inventory[-1]['notice_provenance']={k:pinned[k] for k in ['commit','source','sha256']}

# Record runtime Rust dependency licenses, excluding build-only vendored toolchains.
metadata=json.loads(run('cargo','metadata','--format-version','1','--filter-platform','aarch64-apple-darwin','--locked','--offline'))
normal=run('cargo','tree','--workspace','--target','aarch64-apple-darwin','--edges','normal','--prefix','none','--format','{p}','--locked','--offline')
for package in metadata['packages']:
 if package['source'] is None:continue
 if f"{package['name']} v{package['version']}" not in normal:continue
 directory=Path(package['manifest_path']).parent
 pinned=None
 files=[p for p in directory.iterdir() if p.is_file() and any(t in p.name.lower() for t in ['license','copying','notice','copyright'])]
 if not files:
  vcs=json.loads((directory/'.cargo_vcs_info.json').read_text())
  supplied,pinned=pinned_notice(package['name'],package['version'],package['license'],vcs['git']['sha1'])
  files=[supplied]
 evidence('cargo',package['name'],package['version'],package.get('license',''),package.get('repository') or package['source'],files)
 if pinned:inventory[-1]['notice_provenance']={k:pinned[k] for k in ['commit','source','sha256']}
(NOTICES/'inventory.json').write_text(json.dumps(inventory,indent=2)+'\n')
(NOTICES/'README.md').write_text('Third-party dependency metadata and upstream-provided license/notice evidence for this local candidate.\nRuntime JS files and their own notices remain intact. NuGet nuspec metadata and provided notices are retained.\nSQLite is bundled through libsqlite3-sys; upstream SQLite is public domain (https://www.sqlite.org/copyright.html).\nA final license/source provenance review remains a publication gate.\n')
shutil.copytree(NOTICES,RESOURCES/'THIRD_PARTY')
copy(ROOT/'LICENSE',RUNTIME/'LICENSE')
copy(provenance.record_path(ROOT,PROFILE),OUT/'build-record.json')
copy(OUT/'build-record.json',RESOURCES/'build-record.json')
for doc in provenance.PACKAGED_DOCS:copy(ROOT/'docs'/doc,RUNTIME/'docs'/doc)
copy(ROOT/f'target/{PROFILE}/know-code',RUNTIME/'bin/know-code')
manifest={'layout_version':1,'version':VERSION,'artifact_kind':'local-candidate','target':'darwin-arm64','minimum_macos':'15.0','worker_protocol':7,'store_format':8,'analysis_settings_version':1,'resource_control_version':3,'index_lifecycle_version':4,'index_lifecycle_read_versions':[1,2],'store_read_versions':[2,3,4,5,6,7,8],'store_migrate_versions':[3,4,5,6,7],'store_rebuild_versions':[1,2],'build_profile':PROFILE,'telemetry':'none','prerequisites':{'node':'24.x','dotnet_sdk':'10.x; repository selection/compatible Roslyn required'},'providers':[{'language':lang,'status':status,'entrypoint':entry} for lang,status,entry in [('typescript','required','providers/typescript/worker.mjs'),('csharp','required','providers/dotnet/KnowCode.Provider.DotNet.dll'),('rust','experimental-v0.0.3','providers/rust/worker.mjs'),('swift','experimental-v0.0.3','providers/swift/worker.mjs')]]}
(RESOURCES/'runtime.json').write_text(json.dumps(manifest,indent=2)+'\n')
# Only platform libraries and relative self identities may appear in native load commands.
native_inputs=[RUNTIME/'bin/know-code',ROOT/f'target/{PROFILE}/libknow_code_node.dylib',ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib',RESOURCES/'providers/swift/indexstore-reader']
for native in native_inputs:
 assert run('lipo','-archs',native).strip()=='arm64',native
 for line in run('otool','-L',native).splitlines()[1:]:
  dependency=line.strip().split(' (',1)[0]
  assert dependency.startswith(('@rpath/','/usr/lib/','/System/Library/')) and 'sqlite' not in dependency.lower(),(native,dependency)
info=json.loads(run(RUNTIME/'bin/know-code','runtime-info'));assert info['layout']=='bundle' and info['sqlite']['fts5'] and info['profile']==PROFILE and info['analysis_settings_version']==manifest['analysis_settings_version'] and info['resource_control_version']==manifest['resource_control_version'] and all(info[field]==manifest[field] for field in ['worker_protocol','store_format','index_lifecycle_version','index_lifecycle_read_versions','store_read_versions','store_migrate_versions','store_rebuild_versions'])

NODE=STAGE/'node';NODE.mkdir()
for name in ['index.mjs','index.d.ts','package.json','README.md']:copy(ROOT/'bindings/node'/name,NODE/name)
copy(ROOT/f'target/{PROFILE}/libknow_code_node.dylib',NODE/'know_code.node');copy(ROOT/'LICENSE',NODE/'LICENSE');shutil.copytree(NOTICES,NODE/'THIRD_PARTY')
packed=json.loads(run('npm','pack','--ignore-scripts','--json','--pack-destination',OUT,cwd=NODE));node_archive=OUT/packed[0]['filename']
run('dotnet','pack',ROOT/'bindings/dotnet/KnowCode/KnowCode.csproj','--no-build','--no-restore','-c',CONFIG,'-o',OUT,f'-p:KnowCodeNativeLibrary={ROOT}/target/{PROFILE}/libknow_code_ffi.dylib',f'-p:KnowCodeNotices={NOTICES}')
nuget=OUT/f'KnowCode.{VERSION}.nupkg';assert nuget.is_file()
with tarfile.open(node_archive) as archive:
 provenance.require_output(BUILD_RECORD,f'target/{PROFILE}/libknow_code_node.dylib',hashlib.sha256(archive.extractfile('package/know_code.node').read()).hexdigest(),PROFILE)
with zipfile.ZipFile(nuget) as archive:
 for entry,source in [('lib/net10.0/KnowCode.dll',f'bindings/dotnet/KnowCode/bin/{CONFIG}/net10.0/KnowCode.dll'),('runtimes/osx-arm64/native/libknow_code_ffi.dylib',f'target/{PROFILE}/libknow_code_ffi.dylib')]:
  provenance.require_output(BUILD_RECORD,source,hashlib.sha256(archive.read(entry)).hexdigest(),PROFILE)

# Normalize archive metadata; gzip timestamp does not encode the build time.
def archive_filter(info):
 info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0;return info
runtime_archive=OUT/f'know-code-runtime-{VERSION}-darwin-arm64.tar.gz'
with runtime_archive.open('wb') as stream,gzip.GzipFile(fileobj=stream,mode='wb',filename='',mtime=0) as compressed,tarfile.open(fileobj=compressed,mode='w') as archive:
 archive.add(RUNTIME,arcname=RUNTIME.name,filter=archive_filter)
artifacts=[runtime_archive,node_archive,nuget]
# Build provenance records actual source content; do not pretend an uncommitted tree is a release revision.
sources={}
for directory in ['crates','bindings','providers','proto','scripts']:
 for p in (ROOT/directory).rglob('*'):
  if p.is_file() and not any(part in ['bin','obj','node_modules','target'] for part in p.relative_to(ROOT).parts) and p.suffix in ['.rs','.mjs','.ts','.cs','.c','.h','.proto','.toml','.json','.sh','.py','.csproj']:sources[str(p.relative_to(ROOT))]=digest(p)
for name in ['Cargo.toml','Cargo.lock','package.json','package-lock.json','Directory.Build.props','global.json','rust-toolchain.toml','LICENSE','third_party/licenses/index.json']:
 sources[name]=digest(ROOT/name)
revision=subprocess.run(['git','rev-parse','--verify','HEAD'],cwd=ROOT,text=True,capture_output=True)
try:
 if provenance.verify(ROOT,PROFILE)!=BUILD_RECORD:raise provenance.Invalid('Build record changed during packaging.')
 if provenance.package_inputs(ROOT)!=PACKAGE_INPUTS:raise provenance.Invalid('Packaging inputs changed during assembly; rerun scripts/package.py into a fresh output directory.')
except provenance.Invalid as error:raise SystemExit(str(error))
report={'version':VERSION,'candidate':True,'source_revision':revision.stdout.strip() if revision.returncode==0 else None,'build_provenance':{'file':'build-record.json','sha256':digest(OUT/'build-record.json'),'schema_version':BUILD_RECORD['schema_version'],'build_id':BUILD_RECORD['build_id']},'packaging_inputs':PACKAGE_INPUTS,'build_host':{'macos':platform.mac_ver()[0],'machine':platform.machine()},'profile':PROFILE,'runtime':info,'artifacts':[{'file':p.name,'bytes':p.stat().st_size,'sha256':digest(p)} for p in artifacts],'source_content':sources,'toolchains':{'node':run('node','--version').strip(),'dotnet':run('dotnet','--version').strip(),'rust':run('rustc','--version').strip()},'unresolved_license_metadata':[e['name'] for e in inventory if not e['license']],'unsigned_candidate':True}
(OUT/'manifest.json').write_text(json.dumps(report,indent=2)+'\n')
(OUT/'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.name}\n' for p in [*artifacts,OUT/'build-record.json',OUT/'manifest.json']))
print(json.dumps({'out':str(OUT),'artifacts':report['artifacts'],'unresolved_license_metadata':report['unresolved_license_metadata']},indent=2))
