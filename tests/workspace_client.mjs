import {createInterface} from 'node:readline';
import {pathToFileURL} from 'node:url';
const {Workspace,KnowCodeError} = await import(pathToFileURL(process.argv[2]));
const [root,encoded] = process.argv.slice(3);const options=JSON.parse(encoded);
let workspace;
const print=value=>process.stdout.write(JSON.stringify(value)+'\n');
try {
  workspace=await Workspace.open(root,options);
  print({opened:workspace.initialStatus,root:workspace.root,frozen:Object.isFrozen(workspace.initialStatus)});
} catch(error) {
  print({error:{code:error instanceof KnowCodeError?error.code:'unexpected_error',message:error.message}});
  process.exit(0);
}
for await(const line of createInterface({input:process.stdin})) {
  const action=JSON.parse(line);
  try {
    switch(action.op) {
      case 'status': print(await workspace.status());break;
      case 'wait': print(await workspace.waitForIndex({waitMs:action.wait_ms}));break;
      case 'refresh': print(await workspace.refresh({configuration:action.configuration}));break;
      case 'query': print(await workspace.query(action.request));break;
      case 'close': workspace.close();workspace.close();print({closed:true});break;
      case 'close_during_query': {
        const pending=workspace.query(action.request);workspace.close();print(await pending);break;
      }
      case 'mutate_snapshot': {
        let blocked=false;try{workspace.initialStatus.database='/bogus';}catch{blocked=true;}
        print({blocked,root:workspace.root});break;
      }
      default: throw new Error('unknown test action');
    }
  } catch(error) {print({error:{code:error instanceof KnowCodeError?error.code:'unexpected_error',message:error.message}});}
}
workspace.close();
