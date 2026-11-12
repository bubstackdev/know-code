// Internal adapter bounds, not user settings or a second scheduler.
const DEFINITION_WINDOW = 4;
const EXTERNAL_SOURCE_BYTES = 16 * 1024 * 1024;

export async function* orderedDefinitions(items, request) {
  const iterator=items[Symbol.iterator]();
  for (;;) {
    const window=[];
    // Finish pulling before starting work: an iterator error cannot abandon
    // already-started requests. Every started promise is observed, even when a
    // request fails or the consumer stops after one yielded result.
    while(window.length<DEFINITION_WINDOW) {
      const next=iterator.next();
      if(next.done)break;
      window.push(next.value);
    }
    if(!window.length)return;
    const results=await Promise.allSettled(window.map(item=>Promise.resolve().then(()=>request(item))));
    const failure=results.find(result=>result.status==='rejected');
    if(failure)throw failure.reason;
    for(let i=0;i<window.length;i++)yield {item:window[i],definitions:results[i].value};
  }
}

export class ExternalSources {
  constructor(budget=EXTERNAL_SOURCE_BYTES) {this.budget=budget;this.bytes=0;this.sources=new Map();}
  get(uri) {
    const entry=this.sources.get(uri);
    if(!entry)return;
    this.sources.delete(uri);this.sources.set(uri,entry);
    return entry.source;
  }
  set(uri,source) {
    // Account retained source buffers/maps and text; bound entry overhead too.
    // This is cache payload accounting, not a claim about exact V8 heap/RSS.
    const bytes=source.artifact.utf8.length+source.text.length*2+source.offsets.byteLength
      +source.lines.length*8+(uri.length+source.filename.length)*2+512;
    const previous=this.sources.get(uri);
    if(previous){this.bytes-=previous.bytes;this.sources.delete(uri);}
    if(bytes>this.budget)return; // Still usable by this token, never job-retained.
    while(this.sources.size && (this.bytes+bytes>this.budget || this.sources.size>=64)) {
      const oldest=this.sources.keys().next().value;
      this.bytes-=this.sources.get(oldest).bytes;this.sources.delete(oldest);
    }
    this.sources.set(uri,{source,bytes});this.bytes+=bytes;
  }
}
