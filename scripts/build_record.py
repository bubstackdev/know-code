#!/usr/bin/env python3
"""Build and package evidence for the existing local candidate scripts."""
from pathlib import Path
import argparse,fcntl,hashlib,json,os,platform,shutil,subprocess,sys,time,uuid

VERSION=1
ROOT=Path(__file__).resolve().parents[1]
PROJECTS=('providers/dotnet','bindings/dotnet/KnowCode','bindings/dotnet/Smoke')
ROOT_INPUTS=('Cargo.toml','Cargo.lock','package.json','package-lock.json','Directory.Build.props','global.json','rust-toolchain.toml','tests/fixtures/typescript/package.json')
SCRIPT_INPUTS=('scripts/build.sh','scripts/build_record.py','scripts/generate-protocol.sh')
PACKAGED_DOCS=('WORKSPACE_API.md','MCP.md','GRAPH_SOURCE_PROJECTION.md','QUERY_ADMISSION.md','PACKAGING.md','INSTALLATION.md','RESOURCE_CONTROL.md','WORKSPACE_SETTINGS.md','BUILD_PROVENANCE.md','OFFLINE_VALIDATION.md')
SKIP={'target','node_modules','bin','obj','.git','.artifacts','__pycache__'}
FLAGS=('RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','RUSTDOCFLAGS','CC','CXX','AR','CFLAGS','CXXFLAGS','CPPFLAGS','LDFLAGS','LIBSQLITE3_FLAGS','SQLITE_MAX_VARIABLE_NUMBER','SQLITE_MAX_EXPR_DEPTH','MACOSX_DEPLOYMENT_TARGET','SDKROOT','DEVELOPER_DIR','MSBuildSDKsPath','DOTNET_ROOT','NUGET_PACKAGES','CARGO_HOME','CARGO_TARGET_DIR','RUSTUP_HOME','RUSTUP_TOOLCHAIN','NODE_OPTIONS','NODE_PATH')

class Invalid(RuntimeError):
    pass

def fail(message,profile):
    raise Invalid(f'Build provenance invalid: {message}. Run bash scripts/build.sh {profile}, then package again.')

def digest(path):
    with path.open('rb') as stream:return hashlib.file_digest(stream,'sha256').hexdigest()

def files(directory,skip=()):
    if not directory.is_dir():raise Invalid(f'Missing input/output directory: {directory}')
    for base,dirs,names in os.walk(directory,followlinks=False):
        dirs[:]=sorted(d for d in dirs if d not in skip)
        for name in [*dirs,*sorted(names)]:
            path=Path(base)/name
            if path.is_symlink():raise Invalid(f'Unexpected build/package symlink: {path}')
        for name in sorted(names):yield Path(base)/name

def inventory(paths,root):
    result={}
    for path in paths:
        if path.is_symlink() or not path.is_file():raise Invalid(f'Missing/nonregular build input/output: {path}')
        result[path.relative_to(root).as_posix()]=digest(path)
    return result

def source_inputs(root):
    paths=[root/name for name in (*ROOT_INPUTS,*SCRIPT_INPUTS)]
    for name in ('crates','bindings','providers','proto'):
        paths.extend(p for p in files(root/name,SKIP|{'tests'}) if p.suffix not in ('.md','.node') and p.name!='.DS_Store')
    paths.extend(root/name for name in ('.npmrc','NuGet.Config','nuget.config') if (root/name).exists())
    paths.extend(root/'.cargo'/name for name in ('config','config.toml') if (root/'.cargo'/name).is_file())
    return inventory(paths,root)

def package_inputs(root):
    paths=[root/'scripts/package.py',root/'LICENSE',root/'bindings/node/README.md',root/'bindings/dotnet/KnowCode/README.md']
    paths.extend(root/'docs'/name for name in PACKAGED_DOCS)
    paths.extend(files(root/'third_party/licenses'))
    return inventory(paths,root)

def run(command,root,env,capture=True,inherited=(),timeout=1800):
    process=subprocess.run(command,cwd=root,env=env,text=True,capture_output=capture,timeout=timeout,pass_fds=inherited)
    if process.returncode:raise Invalid(f'Command failed ({process.returncode}): {command}\n{process.stdout or ""}\n{process.stderr or ""}')
    return process.stdout.strip() if capture else None

def toolchains(root,env):
    commands={'rust':('rustc','-vV'),'cargo':('cargo','--version'),'node':('node','--version'),'npm':('npm','--version'),'dotnet':('dotnet','--version'),'clang':('xcrun','clang','--version'),'sdk':('xcrun','--show-sdk-version'),'sdk_path':('xcrun','--show-sdk-path')}
    result={'python':{'identity':sys.version,'launcher':str(Path(sys.executable).resolve())}}
    for name,command in commands.items():
        executable=shutil.which(command[0],path=env.get('PATH'))
        if executable is None:raise Invalid(f'Missing {command[0]}: install it separately and put it on PATH.')
        result[name]={'identity':run(list(command),root,env),'launcher':str(Path(executable).resolve())}
    return result

def build_flags(env):
    return {key:value for key,value in sorted(env.items()) if key in FLAGS or key.startswith(('CARGO_PROFILE_','CARGO_BUILD_','CARGO_TARGET_AARCH64_APPLE_DARWIN_','CFLAGS_','CC_','AR_'))}

def runtime_packages(root):
    lock=json.loads((root/'package-lock.json').read_text())['packages'];visited={}
    def visit(name,requiring):
        source=None
        for parent in (requiring,*requiring.parents):
            candidate=parent/'node_modules'/name
            if (candidate/'package.json').is_file():source=candidate;break
            if parent==root:break
        if source is None or not source.resolve().is_relative_to(root):raise Invalid(f'Missing/outside runtime dependency {name} from {requiring}')
        key=source.relative_to(root).as_posix()
        if key in visited:return
        if source.is_symlink():raise Invalid(f'Runtime dependency is a workspace link: {source}')
        package=json.loads((source/'package.json').read_text())
        if package['version']!=lock.get(key,{}).get('version'):raise Invalid(f'Installed runtime dependency differs from package-lock.json: {key}')
        visited[key]=source
        for dependency in package.get('dependencies',{}):visit(dependency,source)
    requiring=root/'providers/typescript'
    for name in json.loads((requiring/'package.json').read_text())['dependencies']:visit(name,requiring)
    return list(visited.values())

def configurations(root,env):
    result={}
    candidates={Path(env.get('CARGO_HOME',Path.home()/'.cargo'))/name for name in ('config','config.toml')}
    for parent in (root,*root.parents):
        candidates.update(parent/'.cargo'/name for name in ('config','config.toml'))
        candidates.update(parent/name for name in ('Directory.Build.props','Directory.Build.targets','Directory.Packages.props'))
    for path in sorted(candidates):
        if path.is_file():result['configuration:'+str(path)]=digest(path)
    return result

def dependencies(root,env):
    # These exact installed bytes can be compiled or copied by publish --no-build.
    result={}
    for package in runtime_packages(root):result.update(inventory(files(package,{'node_modules'}),root))
    metadata=json.loads(run(['cargo','metadata','--locked','--offline','--filter-platform','aarch64-apple-darwin','--format-version','1'],root,env))
    nodes={node['id']:node for node in metadata['resolve']['nodes']};reachable=set();queue=list(metadata['workspace_members'])
    while queue:
        identity=queue.pop()
        if identity in reachable:continue
        reachable.add(identity)
        queue.extend(edge['pkg'] for edge in nodes[identity]['deps'] if any(kind['kind']!='dev' for kind in edge['dep_kinds']))
    packages=[package for package in metadata['packages'] if package['id'] in reachable]
    result['cargo:resolved-packages']=hashlib.sha256(json.dumps(sorted((p['id'],p['version'],p['source']) for p in packages),sort_keys=True).encode()).hexdigest()
    for package in packages:
        directory=Path(package['manifest_path']).parent
        covered=package['id'] in metadata['workspace_members'] and any(directory.is_relative_to(root/prefix) for prefix in ('crates','bindings','providers','proto'))
        if package['source'] is None and covered:continue
        for path in files(directory,{'target','.git','.artifacts','node_modules','__pycache__'}):result[f'cargo:{package["id"]}/{path.relative_to(directory).as_posix()}']=digest(path)
    nuget={}
    for project in PROJECTS:
        obj=root/project/'obj';assets=obj/'project.assets.json'
        generated=[assets,*obj.glob('*.nuget.g.props'),*obj.glob('*.nuget.g.targets'),*obj.glob('*.nuget.dgspec.json')]
        result.update(inventory(generated,root))
        data=json.loads(assets.read_text())
        for library in data['libraries'].values():
            if library.get('type')!='package':continue
            candidates=[Path(folder)/library['path'] for folder in data['packageFolders']]
            directory=next((p for p in candidates if p.is_dir()),None)
            if directory is None:raise Invalid(f'Missing restored NuGet package {library["path"]}')
            nuget[library['path']]=directory
    for name,directory in sorted(nuget.items()):
        for path in files(directory):result[f'nuget:{name}/{path.relative_to(directory).as_posix()}']=digest(path)
    return result

def record_path(root,profile):return root/'.artifacts/build'/profile/'build-record.json'
def pending_path(root,profile):return record_path(root,profile).with_name('in-progress.json')

def write(path,data):
    path.parent.mkdir(parents=True,exist_ok=True);temporary=path.with_name(path.name+'.next')
    temporary.write_text(json.dumps(data,sort_keys=True,indent=2)+'\n');temporary.replace(path)

def lock(root,shared=False):
    path=root/'.artifacts/build/producer.lock';path.parent.mkdir(parents=True,exist_ok=True)
    stream=path.open('a+b')
    try:fcntl.flock(stream,fcntl.LOCK_NB|(fcntl.LOCK_SH if shared else fcntl.LOCK_EX))
    except BlockingIOError:
        stream.close();raise Invalid('Another build/package operation owns this checkout; retry after it finishes.')
    return stream

def unchanged(expected,actual,what,profile):
    if expected!=actual:
        changed=sorted(key for key in expected.keys()|actual.keys() if expected.get(key)!=actual.get(key))
        fail(f'{what} changed ({", ".join(changed[:4])})',profile)

def begin(root,profile,env):
    if env.get('CARGO_BUILD_TARGET'):fail('CARGO_BUILD_TARGET must be unset for the native candidate build',profile)
    revision=subprocess.run(['git','rev-parse','--verify','HEAD'],cwd=root,text=True,capture_output=True)
    record={'schema_version':VERSION,'status':'building','build_id':uuid.uuid4().hex,'profile':profile,'started_ns':time.time_ns(),'source_revision':revision.stdout.strip() if revision.returncode==0 else None,'build_host':{'macos':platform.mac_ver()[0],'machine':platform.machine()},'source_inputs':source_inputs(root),'configuration_inputs':configurations(root,env),'toolchains':toolchains(root,env),'build_flags':build_flags(env),'build_options':{'cargo_profile':'dev' if profile=='debug' else 'release','dotnet_configuration':profile.capitalize(),'UseSharedCompilation':False,'swift_optimization':'-O0' if profile=='debug' else '-O2','minimum_macos':'15.0'}}
    write(pending_path(root,profile),record);return record

def prepare(root,profile,env,record):
    unchanged(record['source_inputs'],source_inputs(root),'build inputs during restoration',profile)
    unchanged(record['configuration_inputs'],configurations(root,env),'configuration inputs during restoration',profile)
    record['dependency_inputs']=dependencies(root,env);write(pending_path(root,profile),record)

def cargo_build(root,profile,env,inherited=()):
    command=['cargo','build','--workspace','--locked','--profile','dev' if profile=='debug' else 'release','--message-format=json-render-diagnostics']
    logfile=record_path(root,profile).with_name('cargo-build.jsonl')
    with logfile.open('w') as output:
        process=subprocess.Popen(command,cwd=root,env=env,text=True,stdout=subprocess.PIPE,pass_fds=inherited)
        for line in process.stdout:
            output.write(line)
            message=json.loads(line)
            if message.get('reason')=='compiler-message':print(message['message'].get('rendered') or message['message']['message'],file=sys.stderr,end='')
        if process.wait():raise Invalid(f'Cargo build failed; see {logfile}')

def generated_inputs(root,profile):
    paths=[root/'providers/dotnet/obj/protocol/Analysis.cs'];dirs=[];reported=set();complete=False
    log=record_path(root,profile).with_name('cargo-build.jsonl')
    paths.append(log)
    for line in log.read_text().splitlines():
        message=json.loads(line)
        if message.get('reason')=='build-finished':complete=message.get('success') is True
        if message.get('reason')=='compiler-artifact':reported.update(message['filenames'])
        if message.get('reason')=='build-script-executed' and '#know-code-core@' in message['package_id']:
            directory=Path(message['out_dir'])
            if not directory.is_relative_to(root/f'target/{profile}/build'):raise Invalid(f'Cargo generated output escaped the selected profile: {directory}')
            dirs.append(directory)
    if not dirs:raise Invalid('Cargo did not identify the core protocol generation directory')
    required={str(root/f'target/{profile}'/name) for name in ('know-code','libknow_code_node.dylib','libknow_code_ffi.dylib')}
    if not complete or not required<=reported:raise Invalid('Cargo did not report a successful build of every required native output')
    for directory in dirs:paths.extend(directory.rglob('*.rs'))
    for project in PROJECTS:paths.extend((root/project/'obj'/profile.capitalize()/'net10.0').rglob('*.cs'))
    return inventory(paths,root)

def outputs(root,profile):
    paths=[root/f'target/{profile}'/name for name in ('know-code','libknow_code_node.dylib','libknow_code_ffi.dylib')]
    paths.append(root/f'.artifacts/build/{profile}/swift/indexstore-reader')
    paths.extend(files(root/f'.artifacts/build/{profile}/node'))
    for project,assembly in zip(PROJECTS,('KnowCode.Provider.DotNet','KnowCode','Smoke')):
        paths.extend(root/project/'bin'/profile.capitalize()/'net10.0'/(assembly+suffix) for suffix in ('.dll','.deps.json'))
        paths.extend(files(root/project/'bin'/profile.capitalize()/'net10.0'))
        # Publish creates its own deps JSON here; actual bin/runtime payloads
        # remain frozen and the published bytes are checked against them.
        paths.extend(p for p in files(root/project/'obj'/profile.capitalize()/'net10.0') if p.suffix in ('.dll','.pdb') or p.name=='apphost')
    result=inventory(paths,root)
    node=result[f'target/{profile}/libknow_code_node.dylib'];ffi=result[f'target/{profile}/libknow_code_ffi.dylib']
    if result[f'.artifacts/build/{profile}/node/know_code.node']!=node or result[f'bindings/dotnet/Smoke/bin/{profile.capitalize()}/net10.0/libknow_code_ffi.dylib']!=ffi:fail('native staging contains mixed outputs',profile)
    return result

def require_output(record,name,actual_digest,profile):
    if record['outputs'].get(name)!=actual_digest:fail(f'packaged payload does not match the built output {name}',profile)

def finish(root,profile,env,record):
    pending=json.loads(pending_path(root,profile).read_text())
    if pending['build_id']!=record['build_id']:fail('a different producer replaced this build attempt',profile)
    unchanged(record['source_inputs'],source_inputs(root),'build inputs during compilation',profile)
    unchanged(record['configuration_inputs'],configurations(root,env),'configuration inputs during compilation',profile)
    unchanged(record['dependency_inputs'],dependencies(root,env),'dependency inputs during compilation',profile)
    unchanged(record['toolchains'],toolchains(root,env),'toolchains during compilation',profile)
    unchanged(record['build_flags'],build_flags(env),'flags during compilation',profile)
    record.update(status='complete',completed_ns=time.time_ns(),generated_inputs=generated_inputs(root,profile),outputs=outputs(root,profile))
    write(record_path(root,profile),record);pending_path(root,profile).unlink();return record

def verify(root,profile,env=None):
    env=env or dict(os.environ)
    try:
        if pending_path(root,profile).exists():fail('an unfinished build attempt is present',profile)
        record=json.loads(record_path(root,profile).read_text())
        if not isinstance(record,dict) or record.get('schema_version')!=VERSION or record.get('status')!='complete' or record.get('profile')!=profile:fail('record is incomplete or belongs to a different profile/version',profile)
        maps=('source_inputs','configuration_inputs','dependency_inputs','generated_inputs','outputs','toolchains','build_flags','build_options','build_host')
        if any(not isinstance(record.get(name),dict) for name in maps) or any(name not in record for name in ('build_id','started_ns','completed_ns','source_revision')):fail('record is missing or has malformed producer metadata',profile)
        unchanged(record['source_inputs'],source_inputs(root),'build inputs',profile)
        unchanged(record['configuration_inputs'],configurations(root,env),'configuration inputs',profile)
        unchanged(record['dependency_inputs'],dependencies(root,env),'dependency inputs',profile)
        unchanged(record['generated_inputs'],generated_inputs(root,profile),'generated inputs',profile)
        unchanged(record['outputs'],outputs(root,profile),'built outputs',profile)
        unchanged(record['toolchains'],toolchains(root,env),'toolchains used by packaging',profile)
        return record
    except (OSError,KeyError,ValueError) as error:fail(f'missing or malformed record/evidence: {error}',profile)
    except Invalid as error:
        if str(error).startswith('Build provenance invalid:'):raise
        fail(str(error),profile)

def build(root,profile):
    env=dict(os.environ,PATH=os.pathsep.join([str(Path.home()/'.cargo/bin'),str(Path.home()/'.dotnet'),str(Path.home()/'.local/bin'),os.environ.get('PATH','')]),DOTNET_CLI_TELEMETRY_OPTOUT='1',DOTNET_NOLOGO='1',MSBUILDDISABLENODEREUSE='1',DOTNET_CLI_USE_MSBUILD_SERVER='0',MACOSX_DEPLOYMENT_TARGET='15.0',CARGO_TARGET_DIR=str(root/'target'))
    with lock(root) as producer:
        # A writer subprocess keeps the fence if the Python driver dies while
        # that command is still running. Do not certify/reuse orphaned output.
        inherited=(producer.fileno(),)
        record=begin(root,profile,env)
        config=profile.capitalize()
        run(['npm','ci','--ignore-scripts','--no-audit','--no-fund'],root,env,False,inherited)
        projects=(PROJECTS[0]+'/KnowCode.Provider.DotNet.csproj',PROJECTS[1]+'/KnowCode.csproj',PROJECTS[2]+'/Smoke.csproj')
        for project in projects:run(['dotnet','restore',project,'--locked-mode','--nologo'],root,env,False,inherited)
        run(['cargo','metadata','--locked','--filter-platform','aarch64-apple-darwin','--format-version','1'],root,env,inherited=inherited)
        prepare(root,profile,env,record)
        cargo_build(root,profile,env,inherited)
        run(['bash','providers/swift/build.sh',profile],root,env,False,inherited)
        run(['bash','scripts/generate-protocol.sh'],root,env,False,inherited)
        for project in projects:run(['dotnet','build',project,'--no-restore','--nologo','--configuration',config,'-p:UseSharedCompilation=false'],root,env,False,inherited)
        stage=root/f'.artifacts/build/{profile}/node';stage.mkdir(parents=True,exist_ok=True)
        for name in ('index.mjs','index.d.ts','smoke.mjs','package.json'):shutil.copy2(root/'bindings/node'/name,stage/name)
        shutil.copy2(root/f'target/{profile}/libknow_code_node.dylib',stage/'know_code.node')
        shutil.copy2(root/f'target/{profile}/libknow_code_ffi.dylib',root/f'bindings/dotnet/Smoke/bin/{config}/net10.0/')
        if profile=='debug':shutil.copy2(root/'target/debug/libknow_code_node.dylib',root/'bindings/node/know_code.node')
        finish(root,profile,env,record)
        print(f'Build record: {record_path(root,profile)}')

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('operation',choices=('build','verify'));parser.add_argument('profile',choices=('debug','release'));args=parser.parse_args()
    try:
        if args.operation=='build':build(ROOT,args.profile)
        else:
            with lock(ROOT,shared=True):print(json.dumps(verify(ROOT,args.profile),sort_keys=True))
    except Invalid as error:raise SystemExit(str(error))
