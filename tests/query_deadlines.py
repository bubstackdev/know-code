"""Real native host queues consume a query's budget before Rust execution."""
from pathlib import Path
import json,os,subprocess,time
ROOT=Path(__file__).resolve().parents[1]
PROFILE=os.environ.get('KNOW_CODE_BUILD_PROFILE','debug');CONFIG=PROFILE.capitalize()
RUN=ROOT/'.artifacts/query-deadlines'/str(time.time_ns());RUN.mkdir(parents=True)
ENV=dict(os.environ,UV_THREADPOOL_SIZE='1',DOTNET_CLI_TELEMETRY_OPTOUT='1')
ENV['KNOW_CODE_QUERY_SCOPE']=str(RUN/'query-admission')
commands=[['node',str(ROOT/'tests/query_deadlines.mjs'),str(ROOT/f'.artifacts/build/{PROFILE}/node/index.mjs'),str(RUN/'missing.db')],
          ['dotnet',str(ROOT/f'bindings/dotnet/Smoke/bin/{CONFIG}/net10.0/Smoke.dll'),'--deadline-queue',str(RUN/'missing.db')]]
checks=[]
for command in commands:
 p=subprocess.run(command,cwd=ROOT,env=ENV,text=True,capture_output=True,timeout=30)
 (RUN/(command[0]+'.log')).write_text(p.stdout+p.stderr)
 assert p.returncode==0,(command,p.stdout,p.stderr)
 result=json.loads(p.stdout);assert result['passed'];checks.append(result['check'])
summary={'passed':True,'profile':PROFILE,'run':str(RUN),'checks':checks}
(RUN/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary))
