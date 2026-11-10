#!/usr/bin/env python3
"""Assemble local candidate artifacts from matching built outputs; never publish."""
from pathlib import Path
import argparse,gzip,hashlib,json,os,platform,shutil,subprocess,tarfile,time,tomllib,xml.etree.ElementTree as ET,zipfile
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--profile',choices=['debug','release'],default='release');parser.add_argument('--out',type=Path)
args=parser.parse_args();PROFILE=args.profile;CONFIG=PROFILE.capitalize();VERSION=tomllib.loads((ROOT/'Cargo.toml').read_text())['workspace']['package']['version']
OUT=(args.out or ROOT/'.artifacts/packages'/PROFILE/str(time.time_ns())).resolve();OUT.mkdir(parents=True,exist_ok=True)
if platform.system()!='Darwin' or platform.machine()!='arm64':raise SystemExit('Build these candidate artifacts on macOS Apple Silicon.')
STAGE=OUT/'staging';STAGE.mkdir();RUNTIME=STAGE/f'know-code-{VERSION}-darwin-arm64';RESOURCES=RUNTIME/'lib/know-code';RESOURCES.mkdir(parents=True);(RUNTIME/'bin').mkdir()
ENV=dict(os.environ,DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',npm_config_cache=str(OUT/'.npm-cache'),npm_config_audit='false',npm_config_fund='false')
def run(*command,cwd=ROOT):
 p=subprocess.run(list(map(str,command)),cwd=cwd,env=ENV,text=True,capture_output=True,timeout=180)
 if p.returncode:raise RuntimeError(f'{command}\n{p.stdout}\n{p.stderr}')
 return p.stdout

def copy(source,dest):
 if source.is_symlink():raise RuntimeError(f'Unexpected source symlink: {source}')
 dest.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(source,dest)

def digest(path):return hashlib.sha256(path.read_bytes()).hexdigest()

# Build-time notice evidence and dependency inventory, retained alongside each artifact.
NOTICES=STAGE/'THIRD_PARTY';NOTICES.mkdir();inventory=[]
def evidence(kind,name,version,license_text,source,files):
 entry={'ecosystem':kind,'name':name,'version':version,'license':license_text,'source':source,'notice_files':[]}
 for file in files:
  if not file.is_file():continue
  suffix=file.suffix if len(file.suffix)<12 else '.txt';dest=NOTICES/'texts'/(digest(file)+suffix)
  if not dest.exists():copy(file,dest)
  entry['notice_files'].append(str(dest.relative_to(NOTICES)))
 inventory.append(entry)

# Runtime JS dependencies come from the installed pinned closure, never workspace links.
visited=set();npm_lock=json.loads((ROOT/'package-lock.json').read_text())['packages']
def npm_package(name,requiring):
 source=None
 for directory in [requiring,*requiring.parents]:
  candidate=directory/'node_modules'/name
  if (candidate/'package.json').is_file():source=candidate;break
  if directory==ROOT:break
 if source is None:raise RuntimeError(f'Missing installed runtime dependency {name} required by {requiring}')
 key=source.relative_to(ROOT).as_posix()
 if key in visited:return
 visited.add(key);manifest=json.loads((source/'package.json').read_text())
 assert manifest['version']==npm_lock[key]['version'],key
 if source.is_symlink():raise RuntimeError(f'Cannot distribute workspace link: {source}')
 # Preserve installed nesting so conflicting versions keep Node's resolution.
 # Recurse declared dependencies explicitly rather than copying incidental/dev trees.
 shutil.copytree(source,RESOURCES/key,symlinks=False,ignore=shutil.ignore_patterns('node_modules'))
 evidence('npm',name,manifest['version'],manifest.get('license',''),f'https://registry.npmjs.org/{name}',[p for p in source.rglob('*') if p.is_file() and any(t in p.name.lower() for t in ['license','notice','copyright'])])
 for dependency in manifest.get('dependencies',{}):npm_package(dependency,source)
provider_package=ROOT/'providers/typescript'
for name in json.loads((provider_package/'package.json').read_text())['dependencies']:npm_package(name,provider_package)
for provider in ['typescript','shared','rust','swift']:
 for source in (ROOT/'providers'/provider).glob('*.mjs'):copy(source,RESOURCES/'providers'/provider/source.name)
copy(ROOT/'proto/analysis.proto',RESOURCES/'proto/analysis.proto')
copy(ROOT/f'.artifacts/build/{PROFILE}/swift/indexstore-reader',RESOURCES/'providers/swift/indexstore-reader')

# Framework-dependent publication keeps SDK/runtime installation external.
run('dotnet','publish',ROOT/'providers/dotnet/KnowCode.Provider.DotNet.csproj','--no-restore','--no-build','--self-contained','false','-p:UseAppHost=false','-c',CONFIG,'-o',RESOURCES/'providers/dotnet')
for pdb in (RESOURCES/'providers/dotnet').rglob('*.pdb'):pdb.unlink()
assets=json.loads((ROOT/'providers/dotnet/obj/project.assets.json').read_text())
for name,library in assets['libraries'].items():
 if library.get('type')!='package':continue
 package,version=name.rsplit('/',1);directory=Path.home()/'.nuget/packages'/library['path'];nuspec=next(directory.glob('*.nuspec'))
 tree=ET.parse(nuspec);metadata=next(e for e in tree.getroot() if e.tag.endswith('metadata'))
 lic=next((e for e in metadata if e.tag.endswith('license')),None);repository=next((e for e in metadata if e.tag.endswith('repository')),None)
 files=[nuspec]+[p for p in directory.rglob('*') if p.is_file() and p.suffix not in ('.nupkg','.sha512') and any(t in p.name.lower() for t in ['license','notice','copyright'])]
 evidence('nuget',package,version,lic.text if lic is not None else '',repository.attrib if repository is not None else {},files)

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
  pinned=json.loads((ROOT/'third_party/licenses/index.json').read_text())[package['name']+'@'+package['version']]
  vcs=json.loads((directory/'.cargo_vcs_info.json').read_text())
  supplied=ROOT/pinned['file'];assert pinned['commit']==vcs['git']['sha1'] and pinned['license']==package['license'] and digest(supplied)==pinned['sha256']
  files=[supplied]
 evidence('cargo',package['name'],package['version'],package.get('license',''),package.get('repository') or package['source'],files)
 if pinned:inventory[-1]['notice_provenance']={k:pinned[k] for k in ['commit','source','sha256']}
(NOTICES/'inventory.json').write_text(json.dumps(inventory,indent=2)+'\n')
(NOTICES/'README.md').write_text('Third-party dependency metadata and upstream-provided license/notice evidence for this local candidate.\nRuntime JS files and their own notices remain intact. NuGet nuspec metadata and provided notices are retained.\nSQLite is bundled through libsqlite3-sys; upstream SQLite is public domain (https://www.sqlite.org/copyright.html).\nA final license/source provenance review remains a publication gate.\n')
shutil.copytree(NOTICES,RESOURCES/'THIRD_PARTY')
copy(ROOT/'LICENSE',RUNTIME/'LICENSE')
for doc in ['WORKSPACE_API.md','MCP.md','PACKAGING.md','INSTALLATION.md','RESOURCE_CONTROL.md','WORKSPACE_SETTINGS.md']:copy(ROOT/'docs'/doc,RUNTIME/'docs'/doc)
copy(ROOT/f'target/{PROFILE}/know-code',RUNTIME/'bin/know-code')
manifest={'layout_version':1,'version':VERSION,'artifact_kind':'local-candidate','target':'darwin-arm64','minimum_macos':'15.0','worker_protocol':7,'store_format':6,'analysis_settings_version':1,'resource_control_version':2,'build_profile':PROFILE,'telemetry':'none','prerequisites':{'node':'24.x','dotnet_sdk':'10.x; repository selection/compatible Roslyn required'},'providers':[{'language':lang,'status':status,'entrypoint':entry} for lang,status,entry in [('typescript','required','providers/typescript/worker.mjs'),('csharp','required','providers/dotnet/KnowCode.Provider.DotNet.dll'),('rust','experimental-v0.0.3','providers/rust/worker.mjs'),('swift','experimental-v0.0.3','providers/swift/worker.mjs')]]}
(RESOURCES/'runtime.json').write_text(json.dumps(manifest,indent=2)+'\n')
# Only platform libraries and relative self identities may appear in native load commands.
native_inputs=[RUNTIME/'bin/know-code',ROOT/f'target/{PROFILE}/libknow_code_node.dylib',ROOT/f'target/{PROFILE}/libknow_code_ffi.dylib',RESOURCES/'providers/swift/indexstore-reader']
for native in native_inputs:
 assert run('lipo','-archs',native).strip()=='arm64',native
 for line in run('otool','-L',native).splitlines()[1:]:
  dependency=line.strip().split(' (',1)[0]
  assert dependency.startswith(('@rpath/','/usr/lib/','/System/Library/')) and 'sqlite' not in dependency.lower(),(native,dependency)
info=json.loads(run(RUNTIME/'bin/know-code','runtime-info'));assert info['layout']=='bundle' and info['sqlite']['fts5'] and info['profile']==PROFILE and info['analysis_settings_version']==manifest['analysis_settings_version'] and info['resource_control_version']==manifest['resource_control_version']

NODE=STAGE/'node';NODE.mkdir()
for name in ['index.mjs','index.d.ts','package.json','README.md']:copy(ROOT/'bindings/node'/name,NODE/name)
copy(ROOT/f'target/{PROFILE}/libknow_code_node.dylib',NODE/'know_code.node');copy(ROOT/'LICENSE',NODE/'LICENSE');shutil.copytree(NOTICES,NODE/'THIRD_PARTY')
packed=json.loads(run('npm','pack','--ignore-scripts','--json','--pack-destination',OUT,cwd=NODE));node_archive=OUT/packed[0]['filename']
run('dotnet','pack',ROOT/'bindings/dotnet/KnowCode/KnowCode.csproj','--no-build','--no-restore','-c',CONFIG,'-o',OUT,f'-p:KnowCodeNativeLibrary={ROOT}/target/{PROFILE}/libknow_code_ffi.dylib',f'-p:KnowCodeNotices={NOTICES}')
nuget=OUT/f'KnowCode.{VERSION}.nupkg';assert nuget.is_file()

# Normalize archive metadata; gzip timestamp does not encode the build time.
def archive_filter(info):
 info.uid=info.gid=0;info.uname=info.gname='';info.mtime=0;return info
runtime_archive=OUT/f'know-code-runtime-{VERSION}-darwin-arm64.tar.gz'
with runtime_archive.open('wb') as stream,gzip.GzipFile(fileobj=stream,mode='wb',filename='',mtime=0) as compressed,tarfile.open(fileobj=compressed,mode='w') as archive:
 archive.add(RUNTIME,arcname=RUNTIME.name,filter=archive_filter)
artifacts=[runtime_archive,node_archive,nuget]
(OUT/'SHA256SUMS').write_text(''.join(f'{digest(p)}  {p.name}\n' for p in artifacts))
# Build provenance records actual source content; do not pretend an uncommitted tree is a release revision.
sources={}
for directory in ['crates','bindings','providers','proto','scripts']:
 for p in (ROOT/directory).rglob('*'):
  if p.is_file() and not any(part in ['bin','obj','node_modules','target'] for part in p.relative_to(ROOT).parts) and p.suffix in ['.rs','.mjs','.ts','.cs','.c','.h','.proto','.toml','.json','.sh','.py','.csproj']:sources[str(p.relative_to(ROOT))]=digest(p)
for name in ['Cargo.toml','Cargo.lock','package.json','package-lock.json','Directory.Build.props','global.json','rust-toolchain.toml','LICENSE','third_party/licenses/index.json']:
 sources[name]=digest(ROOT/name)
revision=subprocess.run(['git','rev-parse','--verify','HEAD'],cwd=ROOT,text=True,capture_output=True)
report={'version':VERSION,'candidate':True,'source_revision':revision.stdout.strip() if revision.returncode==0 else None,'build_host':{'macos':platform.mac_ver()[0],'machine':platform.machine()},'profile':PROFILE,'runtime':info,'artifacts':[{'file':p.name,'bytes':p.stat().st_size,'sha256':digest(p)} for p in artifacts],'source_content':sources,'toolchains':{'node':run('node','--version').strip(),'dotnet':run('dotnet','--version').strip(),'rust':run('rustc','--version').strip()},'unresolved_license_metadata':[e['name'] for e in inventory if not e['license']],'unsigned_candidate':True}
(OUT/'manifest.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps({'out':str(OUT),'artifacts':report['artifacts'],'unresolved_license_metadata':report['unresolved_license_metadata']},indent=2))
