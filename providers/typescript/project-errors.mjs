// Expected user/project setup failures only. Extraction, transport and storage
// exceptions must still fail the staged worker job.
export class ProjectSetupError extends Error {
  constructor(code,message,compilerVersion='unavailable') {
    super(message);this.code=code;this.compilerVersion=compilerVersion;
  }
}
export function boundedText(value,maximum=4096) {
  const bytes=Buffer.from(String(value),'utf8');
  if(bytes.length<=maximum)return bytes.toString('utf8');
  let end=maximum;
  while(end>0&&(bytes[end]&0xc0)===0x80)end--;
  return bytes.toString('utf8',0,end);
}
