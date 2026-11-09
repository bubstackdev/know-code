import path from 'node:path';
import {createHash} from 'node:crypto';
// A compiler view of Box<string>.value is not a second declaration of Box<T>.value.
// Use the compiler's declaration root only when it identifies one real symbol.
// Union/intersection/spread views can have several roots; never pick one branch.
export function declarationSymbols(ts, checker) {
  const canonical = new WeakMap();
  return symbol => {
    if (canonical.has(symbol)) return canonical.get(symbol);
    let result = symbol;
    if (symbol.flags & ts.SymbolFlags.Transient) {
      const roots = checker.getRootSymbols(symbol);
      if (roots.length === 1 && !(roots[0].flags & ts.SymbolFlags.Transient)
          && roots[0].declarations?.length) result = roots[0];
    }
    canonical.set(symbol, result);
    return result;
  };
}

// Opaque public IDs are constructed by the adapter, never parsed by the core.
export function symbolIdentity(root,context,symbol) {
  const declaration=symbol.declarations?.[0];
  const anchor=declaration?path.relative(root,declaration.getSourceFile().fileName).split(path.sep).join('/')+':'+declaration.pos:'$external';
  return `${context}:${anchor}:${symbol.flags}:${symbol.getName()}`;
}

export function syntheticIdentity(root,context,node,kind) {
  return `${context}:$${kind}:${path.relative(root,node.getSourceFile().fileName).split(path.sep).join('/')}:${node.pos}:${node.kind}:${kind}`;
}

// Dispatch keys describe receiver specialization, separately from declaration
// identity. Unsupported/open types remain unknown rather than collapsing to a
// generic declaration and inventing runtime candidates.
export function dispatchTypeKeys(ts,checker,root) {
  const cache=new WeakMap(), specializations=new WeakMap();
  function token(type,seen=new Set()) {
    if(cache.has(type))return cache.get(type);
    if(seen.has(type)||type.flags&(ts.TypeFlags.TypeParameter|ts.TypeFlags.Any|ts.TypeFlags.Unknown|ts.TypeFlags.Conditional|ts.TypeFlags.IndexedAccess))return null;
    seen=new Set([...seen,type]);let result;
    if(type.flags&ts.TypeFlags.EnumLiteral)result=type.symbol?.declarations?.length?['enum',symbolIdentity(root,'$dispatch',type.symbol)]:null;
    else if(type.flags&ts.TypeFlags.BooleanLiteral)result=['boolean',type.intrinsicName];
    else if(type.flags&ts.TypeFlags.BigIntLiteral)result=['bigint',type.value.negative,type.value.base10Value];
    else if(type.flags&ts.TypeFlags.NumberLiteral)result=Number.isFinite(type.value)?['number',type.value]:null;
    else if(type.flags&ts.TypeFlags.StringLiteral)result=['string',type.value];
    else if(type.flags&ts.TypeFlags.UniqueESSymbol)result=type.symbol?.declarations?.length?['unique_symbol',symbolIdentity(root,'$dispatch',type.symbol)]:null;
    else if(type.flags&ts.TypeFlags.UnionOrIntersection) {
      const children=type.types.map(t=>token(t,seen));
      result=children.includes(null)?null:[type.isUnion()?'union':'intersection',...children.map(JSON.stringify).sort()];
    } else if(type.flags&ts.TypeFlags.Object) {
      const symbol=type.symbol;
      const args=type.objectFlags&ts.ObjectFlags.Reference?checker.getTypeArguments(type).slice(0,type.target?.typeParameters?.length??0):[];
      const children=args.map(t=>token(t,seen));
      result=!(type.objectFlags&(ts.ObjectFlags.ClassOrInterface|ts.ObjectFlags.Reference))||!symbol?.declarations?.some(d=>ts.isClassDeclaration(d)||ts.isInterfaceDeclaration(d))||children.includes(null)?null:['named',symbolIdentity(root,'$dispatch',symbol),children];
    } else result=type.flags&(ts.TypeFlags.String|ts.TypeFlags.Number|ts.TypeFlags.Boolean|ts.TypeFlags.BigInt|ts.TypeFlags.ESSymbol|ts.TypeFlags.Void|ts.TypeFlags.Undefined|ts.TypeFlags.Null|ts.TypeFlags.Never|ts.TypeFlags.NonPrimitive)?['intrinsic',type.flags]:null;
    cache.set(type,result);return result;
  }
  return (receiver,declaration) => {
    const owner=declaration?.parent;
    if(!owner || !(ts.isClassDeclaration(owner)||ts.isInterfaceDeclaration(owner)||ts.isClassExpression(owner)))return '';
    const symbol=owner.name&&checker.getSymbolAtLocation(owner.name);if(!symbol)return '';
    const seen=new Set();
    function find(type) {
      if(seen.has(type))return null;seen.add(type);
      if(type.flags&ts.TypeFlags.TypeParameter) {const constraint=checker.getBaseConstraintOfType(type);return constraint?find(constraint):null;}
      if(type.symbol===symbol) {
        const args=type.objectFlags&ts.ObjectFlags.Reference?checker.getTypeArguments(type).slice(0,type.target?.typeParameters?.length??0):[];
        if(specializations.has(type))return specializations.get(type);
        const parts=args.map(t=>token(t));const result=parts.includes(null)?null:parts.length?createHash('sha256').update(JSON.stringify(parts)).digest('hex'):'[]';
        specializations.set(type,result);return result;
      }
      for(const base of compilerBaseTypes(ts,checker,type)) {const result=find(base);if(result!==null)return result;}
      return null;
    }
    return find(receiver)??'';
  };
}

// getBaseTypes accepts declared class/interface types, not arbitrary structural
// object types produced by contextual signatures or object factories.
export function compilerBaseTypes(ts,checker,type) {
  const declaration=type.flags&ts.TypeFlags.Object?(type.objectFlags&ts.ObjectFlags.Reference?type.target:type):undefined;
  return declaration?.objectFlags&ts.ObjectFlags.ClassOrInterface?checker.getBaseTypes(type)??[]:[];
}
