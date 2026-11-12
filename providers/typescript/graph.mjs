import {compilerBaseTypes} from './symbols.mjs';
// TypeScript semantics only. Storage, candidate expansion and walks live in Rust.
export function graphFacts(ts, checker, {emit, synthetic, occurrence, relationship, dispatchKey,
  symbolName = symbol => symbol.getName(), declarations = symbol => symbol.declarations ?? [],
  signatureDeclaration = signature => signature?.declaration, implementationProofs = true}) {
  let file, artifactId, offsets;
  const point = node => ({artifactId, startByte: offsets[node.getStart(file)], endByte: offsets[node.getEnd()]});
  const relation = (sourceId, targetId, kind, node, implicit=false, dispatch='',dispatchKey='') => {
    if (kind === 'contains' && sourceId === targetId) return;
    relationship({sourceId,targetId,kind,resolution:targetId?'compiler_bound':'unresolved',...point(node),implicit,dispatch,dispatchKey});
  };
  const named = node => node.name && checker.getSymbolAtLocation(node.name);
  function functionId(node) {
    if (ts.isConstructorDeclaration(node) || ts.isGetAccessorDeclaration(node) || ts.isSetAccessorDeclaration(node))
      return synthetic(node, ts.isConstructorDeclaration(node)?'constructor':'accessor',
        ts.isConstructorDeclaration(node)?'<constructor>':`${ts.isGetAccessorDeclaration(node)?'get':'set'} ${node.name.getText(node.getSourceFile())}`);
    let symbol=named(node);
    if (!symbol && (ts.isArrowFunction(node)||ts.isFunctionExpression(node))) {
      const parent=node.parent;
      if (ts.isVariableDeclaration(parent)||ts.isPropertyAssignment(parent)||ts.isPropertyDeclaration(parent)) symbol=named(parent);
    }
    return symbol?emit(symbol):synthetic(node,'lambda','<lambda>');
  }
  function implementations(node) {
    const current=checker.getTypeAtLocation(node);
    for(const clause of node.heritageClauses??[]) {
      const kind=clause.token===ts.SyntaxKind.ImplementsKeyword?'implements':'overrides';
      for(const syntax of clause.types) {
        const seen=new Map();
        function base(type,relationKind=kind) {
          if(seen.get(type)?.has(relationKind))return;
          if(!seen.has(type))seen.set(type,new Set());seen.get(type).add(relationKind);
          for(const member of checker.getPropertiesOfType(type)) {
            const implementation=checker.getPropertyOfType(current,symbolName(member));
            if(!implementation)continue;
            const mine=implementation.valueDeclaration??declarations(implementation)[0];
            const theirs=member.valueDeclaration??declarations(member)[0];
            if(!mine||!theirs)continue;
            // Declaration-only heritage is explicit evidence. Never discover
            // arbitrary structurally similar classes across the workspace.
            if(!checker.isTypeAssignableTo(checker.getTypeOfSymbolAtLocation(implementation,mine),checker.getTypeOfSymbolAtLocation(member,theirs)))continue;
            const methods=d=>ts.isMethodDeclaration(d)||ts.isMethodSignature(d);
            if(methods(mine)&&methods(theirs))relation(functionId(mine),functionId(theirs),relationKind,syntax,false,'',dispatchKey(type,theirs));
            else if((ts.isGetAccessorDeclaration(mine)||ts.isSetAccessorDeclaration(mine)) && (ts.isGetAccessorDeclaration(theirs)||ts.isSetAccessorDeclaration(theirs))) {
              for(const a of declarations(implementation))for(const b of declarations(member))
                if(a.kind===b.kind&&(ts.isGetAccessorDeclaration(a)||ts.isSetAccessorDeclaration(a)))relation(functionId(a),functionId(b),relationKind,syntax,false,'',dispatchKey(type,b));
            }
          }
          if(type.flags&ts.TypeFlags.Object) {
            for(const ancestor of compilerBaseTypes(ts,checker,type))base(ancestor,relationKind);
            // An override in a subclass may also implement interfaces explicitly
            // declared by its ancestors. Keep the compiler's ancestry, not a
            // workspace-wide structural/name scan.
            for(const declaration of type.symbol?.declarations??[])
              for(const heritage of declaration.heritageClauses??[])
                if(heritage.token===ts.SyntaxKind.ImplementsKeyword)
                  for(const contract of heritage.types)base(checker.getTypeAtLocation(contract),'implements');
          }
        }
        base(checker.getTypeAtLocation(syntax));
      }
    }
  }
  function scope(node, context) {
    if (ts.isSourceFile(node)) {
      const module=checker.getSymbolAtLocation(node);
      const owner=module?emit(module):synthetic(node,'module','<script>');
      const execution=synthetic(node,'initializer','<module initialization>');
      relation(owner,execution,'initializes',node);
      return {owner,execution,initialization:true};
    }
    if (ts.isFunctionLike(node) && (node.body || ts.isMethodSignature(node))) {
      const owner=functionId(node);
      if(context.owner!==owner)relation(context.owner,owner,'contains',node.name??node);
      if(!named(node)||ts.isConstructorDeclaration(node)||ts.isGetAccessorDeclaration(node)||ts.isSetAccessorDeclaration(node))
        occurrence(owner,node.name??node,'declaration');
      return {owner,execution:owner,initialization:false};
    }
    if ((ts.isClassDeclaration(node)||ts.isInterfaceDeclaration(node)||ts.isTypeAliasDeclaration(node)||ts.isEnumDeclaration(node)) && named(node)) {
      const owner=emit(named(node));relation(context.owner,owner,'contains',node.name);
      if(implementationProofs&&(ts.isClassDeclaration(node)||ts.isInterfaceDeclaration(node)))implementations(node);
      return {...context,owner};
    }
    if (ts.isVariableDeclaration(node)||ts.isPropertyDeclaration(node)) {
      const symbol=named(node);
      if(!symbol)return context;
      const owner=emit(symbol);relation(context.owner,owner,'contains',node.name);
      if(node.initializer && (ts.isPropertyDeclaration(node)||context.initialization)) {
        const execution=synthetic(node,'initializer',`<initialize ${symbolName(symbol)}>`);
        relation(owner,execution,'initializes',node.name);
        return {owner,execution,initialization:true};
      }
      return {...context,owner};
    }
    if (ts.isClassStaticBlockDeclaration(node)) {
      const owner=synthetic(node,'initializer','<static initialization>');
      relation(context.owner,owner,'initializes',node);return {owner,execution:owner,initialization:true};
    }
    return context;
  }
  // Follow only transparent assignment-target wrappers. Receiver/index/key
  // expressions remain reads; nested object/tuple destinations are writes.
  function accessDirection(access) {
    let target=access;
    for (;;) {
      const parent=target.parent;
      if ((ts.isParenthesizedExpression(parent)||ts.isSpreadElement(parent))&&parent.expression===target
          ||ts.isArrayLiteralExpression(parent)&&parent.elements.includes(target)
          ||ts.isPropertyAssignment(parent)&&parent.initializer===target
          ||ts.isObjectLiteralExpression(parent)&&parent.properties.includes(target)) target=parent;
      else break;
    }
    const parent=target.parent;
    const assigned=ts.isBinaryExpression(parent)&&parent.left===target
      &&parent.operatorToken.kind>=ts.SyntaxKind.FirstAssignment&&parent.operatorToken.kind<=ts.SyntaxKind.LastAssignment;
    const iteration=(ts.isForInStatement(parent)||ts.isForOfStatement(parent))&&parent.initializer===target;
    const increment=(ts.isPrefixUnaryExpression(parent)||ts.isPostfixUnaryExpression(parent))
      &&[ts.SyntaxKind.PlusPlusToken,ts.SyntaxKind.MinusMinusToken].includes(parent.operator);
    return {write:assigned||iteration||increment,
      read:increment||(!iteration&&(!assigned||parent.operatorToken.kind!==ts.SyntaxKind.EqualsToken))};
  }
  function accessorCalls(access,symbol,context,site) {
    if (!(symbol.flags&ts.SymbolFlags.Accessor)) return;
    const {read,write}=accessDirection(access);
    for(const declaration of declarations(symbol)) {
      if(read&&ts.isGetAccessorDeclaration(declaration)||write&&ts.isSetAccessorDeclaration(declaration))
        relation(context.execution,functionId(declaration),'call',site,true,
          access.expression.kind===ts.SyntaxKind.SuperKeyword?'direct':'virtual',
          dispatchKey(checker.getTypeAtLocation(access.expression),declaration));
    }
  }
  function reference(node, symbol, context, declaration) {
    const target=emit(symbol);
    if(declaration) {relation(context.owner,target,'contains',node);return;}
    const kind=symbol.flags&(ts.SymbolFlags.Type|ts.SymbolFlags.Namespace)?'type_reference':'reference';
    relation(context.owner,!declarations(symbol).length&&(symbol.flags&ts.SymbolFlags.Transient)?'':target,kind,node);
    if (ts.isPropertyAccessExpression(node.parent)&&node.parent.name===node)
      accessorCalls(node.parent,symbol,context,node);
  }
  function operation(node, context) {
    if(ts.isElementAccessExpression(node)) {
      const symbol=checker.getSymbolAtLocation(node)??checker.getSymbolAtLocation(node.argumentExpression);
      if(symbol?.flags&ts.SymbolFlags.Accessor) {
        relation(context.owner,emit(symbol),'reference',node.argumentExpression);
        accessorCalls(node,symbol,context,node.argumentExpression);
      }
    }
    if(ts.isCallExpression(node)||ts.isNewExpression(node)||ts.isTaggedTemplateExpression(node)) {
      const signature=checker.getResolvedSignature(node);
      const declaration=signatureDeclaration(signature);
      let expression=ts.isTaggedTemplateExpression(node)?node.tag:node.expression;
      while(ts.isParenthesizedExpression(expression))expression=expression.expression;
      let binding=checker.getSymbolAtLocation(expression);
      if(binding?.flags&ts.SymbolFlags.Alias)binding=checker.getAliasedSymbol(binding);
      // A signature's declaration can be the function that originally supplied
      // a mutable variable's *type*. It is not proof of that variable's value.
      const constantFunction=declaration&&(ts.isArrowFunction(declaration)||ts.isFunctionExpression(declaration))
        &&ts.isVariableDeclaration(declaration.parent)&&declaration.parent.initializer===declaration
        &&(declaration.parent.parent.flags&ts.NodeFlags.Const)
        &&named(declaration.parent)===binding;
      const direct=!!(binding?.flags&(ts.SymbolFlags.Function|ts.SymbolFlags.Method|ts.SymbolFlags.Class))
        ||expression===declaration||constantFunction;
      let target='';let dispatch='direct';
      if(direct && declaration && (ts.isFunctionDeclaration(declaration)||ts.isFunctionExpression(declaration)||ts.isArrowFunction(declaration)
          ||ts.isMethodDeclaration(declaration)||ts.isMethodSignature(declaration)||ts.isConstructorDeclaration(declaration))) {
        target=functionId(declaration);
        if(ts.isMethodSignature(declaration))dispatch='interface';
        else if(ts.isMethodDeclaration(declaration)&&!declaration.modifiers?.some(m=>m.kind===ts.SyntaxKind.StaticKeyword))dispatch='virtual';
      } else if(direct && ts.isNewExpression(node)) {
        let symbol=checker.getSymbolAtLocation(node.expression);
        if(symbol?.flags&ts.SymbolFlags.Alias)symbol=checker.getAliasedSymbol(symbol);
        if(symbol?.flags&ts.SymbolFlags.Class)target=emit(symbol);
      }
      if(ts.isPropertyAccessExpression(expression)&&expression.expression.kind===ts.SyntaxKind.SuperKeyword)dispatch='direct';
      relation(context.execution,target,'call',node,false,target?dispatch:'indirect',ts.isPropertyAccessExpression(expression)&&declaration?dispatchKey(checker.getTypeAtLocation(expression.expression),declaration):'');
    }
    if(ts.isImportDeclaration(node)||ts.isExportDeclaration(node)) {
      if(node.moduleSpecifier) {
        const target=checker.getSymbolAtLocation(node.moduleSpecifier);
        relation(context.owner,target?emit(target):'','imports',node.moduleSpecifier);
      }
    }
    if(ts.isExpressionWithTypeArguments(node)&&ts.isHeritageClause(node.parent)) {
      let target=checker.getSymbolAtLocation(node.expression);
      if(target?.flags&ts.SymbolFlags.Alias)target=checker.getAliasedSymbol(target);
      relation(context.owner,target?emit(target):'','inherits',node);
    }
  }
  return {start(source,artifact,map){file=source;artifactId=artifact;offsets=map;},scope,reference,operation};
}
