using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Operations;
using KnowCode.Protocol;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

static partial class Analysis
{
    // Compiler facts only. Shared Rust operators expand candidates and walk paths.
    private sealed class GraphFacts(SemanticModel model, Source source, Emitter emitter)
    {
        private readonly Emitter output = emitter;
        private readonly Dictionary<SyntaxNode, string> initializers = [];
        private IMethodSymbol? PrimaryConstructor(TypeDeclarationSyntax type)
        {
            // Select by the compiler's declaring syntax, never by a constructor
            // name/signature guess (which could select a secondary constructor).
            var constructors = model.GetDeclaredSymbol(type)?.InstanceConstructors.Where(c =>
                c.DeclaringSyntaxReferences.Any(r => r.SyntaxTree == type.SyntaxTree && r.Span == type.Span)).ToArray();
            return constructors is { Length: 1 } ? constructors[0] : null;
        }
        public string Owner(SyntaxNode node)
        {
            foreach (var ancestor in node.AncestorsAndSelf())
            {
                if (ancestor is PrimaryConstructorBaseTypeSyntax && ancestor.Parent?.Parent is TypeDeclarationSyntax primaryType
                    && PrimaryConstructor(primaryType) is { } constructor)
                    return output.Symbol(Normalize(constructor));
                if (ancestor is EqualsValueClauseSyntax init &&
                    (init.Parent is PropertyDeclarationSyntax || init.Parent is VariableDeclaratorSyntax { Parent.Parent: BaseFieldDeclarationSyntax }))
                {
                    if (!initializers.TryGetValue(init, out var id))
                    {
                        var declared = init.Parent is VariableDeclaratorSyntax variable ? model.GetDeclaredSymbol(variable) : model.GetDeclaredSymbol(init.Parent!);
                        id = output.Synthetic(source, init, "initializer", "<field/property initialization>", declared is null ? null : Normalize(declared));
                        initializers.Add(init, id);
                        if (declared is not null) Edge(output.Symbol(Normalize(declared)), id, "initializes", init);
                    }
                    return id;
                }
                if (ancestor is AnonymousFunctionExpressionSyntax lambda && model.GetOperation(lambda) is IAnonymousFunctionOperation anonymous)
                    return output.Symbol(Normalize(anonymous.Symbol));
                if (ancestor is BaseMethodDeclarationSyntax or LocalFunctionStatementSyntax or AccessorDeclarationSyntax)
                {
                    var symbol = model.GetDeclaredSymbol(ancestor);
                    if (symbol is not null) return output.Symbol(Normalize(symbol));
                }
                if (ancestor is PropertyDeclarationSyntax { ExpressionBody: not null } property && model.GetDeclaredSymbol(property)?.GetMethod is { } getter)
                    return output.Symbol(Normalize(getter));
                if (ancestor is IndexerDeclarationSyntax { ExpressionBody: not null } indexer && model.GetDeclaredSymbol(indexer)?.GetMethod is { } indexGetter)
                    return output.Symbol(Normalize(indexGetter));
                if (ancestor is GlobalStatementSyntax) break;
                if (ancestor is BaseTypeDeclarationSyntax or BaseNamespaceDeclarationSyntax)
                {
                    var symbol = model.GetDeclaredSymbol(ancestor);
                    if (symbol is not null) return output.Symbol(Normalize(symbol));
                }
            }
            return output.Synthetic(source, node.SyntaxTree.GetRoot(), "initializer", "<file initialization>");
        }
        private void Edge(string from, string? to, string kind, SyntaxNode node, bool implicitCall = false, string dispatch = "", string dispatchKey = "")
        {
            if (kind == "contains" && from == to) return;
            output.Relationship(new Relationship {SourceId=from,TargetId=to??"",Kind=kind,
                Resolution=to is null?"unresolved":"compiler_bound", ArtifactId=source.Id,
                StartByte=(ulong)source.Offsets[node.SpanStart],EndByte=(ulong)source.Offsets[node.Span.End],
                Implicit=implicitCall,Dispatch=dispatch,DispatchKey=dispatchKey});
        }
        private readonly Dictionary<INamedTypeSymbol,string> dispatchKeys=new(SymbolEqualityComparer.Default);
        private string DispatchKey(IMethodSymbol? method)
        {
            if(method?.ContainingType is not { } type)return "";
            static bool Open(ITypeSymbol t) => t.TypeKind==TypeKind.Error || t is ITypeParameterSymbol
                ||t is IArrayTypeSymbol a&&Open(a.ElementType)
                ||t is INamedTypeSymbol n&&(n.TypeArguments.Any(Open)||n.ContainingType is { } parent&&Open(parent));
            if(dispatchKeys.TryGetValue(type,out var cached))return cached;
            if(Open(type))return dispatchKeys[type]="";
            var arguments=new List<ITypeSymbol>();
            for(var receiver=type;receiver is not null;receiver=receiver.ContainingType)arguments.AddRange(receiver.TypeArguments);
            if(arguments.Count==0)return dispatchKeys[type]="$";
            var reference=DocumentationCommentId.CreateReferenceId(type);
            if(reference is null)return dispatchKeys[type]="";
            var evidence=new List<string>{reference};
            void Assembly(ITypeSymbol argument) {
                if(argument.TypeKind==TypeKind.Dynamic)argument=model.Compilation.GetSpecialType(SpecialType.System_Object);
                evidence.Add(argument.ContainingAssembly?.Identity.ToString()??"");
                if(argument is IArrayTypeSymbol array)Assembly(array.ElementType);
                if(argument is INamedTypeSymbol named) {foreach(var item in named.TypeArguments)Assembly(item);if(named.ContainingType is { } parent)Assembly(parent);}
            }
            foreach(var argument in arguments)Assembly(argument);
            return dispatchKeys[type]=Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(JsonSerializer.Serialize(evidence)))).ToLowerInvariant();
        }
        private string Lambda(IAnonymousFunctionOperation operation)
        {
            var id=output.Symbol(Normalize(operation.Symbol));
            output.Occurrence(id,source,operation.Syntax.Span,"declaration",operation.Syntax.Span);
            return id;
        }
        public void Declaration(SyntaxNode node, ISymbol declared)
        {
            var id=output.Symbol(Normalize(declared));
            if(node.Parent is { } parent) Edge(Owner(parent),id,"contains",node);
            if(declared is IMethodSymbol method)
                for(var overridden=method.OverriddenMethod;overridden is not null;overridden=overridden.OverriddenMethod)
                    Edge(id,output.Symbol(Normalize(overridden)),"overrides",node,dispatchKey:DispatchKey(overridden));
            if(node is BaseTypeDeclarationSyntax typeSyntax && declared is INamedTypeSymbol type)
            {
                if(typeSyntax.BaseList is { } bases)
                    foreach(var syntax in bases.Types)
                        if(model.GetTypeInfo(syntax.Type).Type is { } baseType)
                            Edge(id,output.Symbol(Normalize(baseType)),"inherits",syntax.Type);
                // Interface mapping can name a base virtual slot. Select its
                // most-derived compiler-proven override in this receiver type.
                var overrides=new Dictionary<IMethodSymbol,IMethodSymbol>(SymbolEqualityComparer.Default);
                for(var receiver=type.AllInterfaces.IsEmpty?null:type;receiver is not null;receiver=receiver.BaseType)
                {
                    var methods=receiver.GetMembers().OfType<IMethodSymbol>().Concat(receiver.GetMembers().OfType<IPropertySymbol>().SelectMany(p=>new[]{p.GetMethod,p.SetMethod}.OfType<IMethodSymbol>()));
                    foreach(var candidate in methods)
                        for(var ancestor=candidate.OverriddenMethod;ancestor is not null;ancestor=ancestor.OverriddenMethod)
                            overrides.TryAdd((IMethodSymbol)Normalize(ancestor),candidate);
                }
                IMethodSymbol Effective(IMethodSymbol method) => overrides.TryGetValue((IMethodSymbol)Normalize(method),out var selected)?selected:method;
                foreach(var iface in type.AllInterfaces)
                    foreach(var member in iface.GetMembers())
                    {
                        var implementation=type.FindImplementationForInterfaceMember(member);
                        if(member is IMethodSymbol m && implementation is IMethodSymbol i)
                            Edge(output.Symbol(Normalize(Effective(i))),output.Symbol(Normalize(m)),"implements",typeSyntax,dispatchKey:DispatchKey(m));
                        if(member is IPropertySymbol p && implementation is IPropertySymbol q)
                        {
                            if(p.GetMethod is { } pg && q.GetMethod is { } qg) Edge(output.Symbol(Normalize(Effective(qg))),output.Symbol(Normalize(pg)),"implements",typeSyntax,dispatchKey:DispatchKey(pg));
                            if(p.SetMethod is { } ps && q.SetMethod is { } qs) Edge(output.Symbol(Normalize(Effective(qs))),output.Symbol(Normalize(ps)),"implements",typeSyntax,dispatchKey:DispatchKey(ps));
                        }
                    }
                foreach(var property in type.GetMembers().OfType<IPropertySymbol>())
                    for(var baseProperty=property.OverriddenProperty;baseProperty is not null;baseProperty=baseProperty.OverriddenProperty)
                    {
                        if(property.GetMethod is { } get && baseProperty.GetMethod is { } baseGet) Edge(output.Symbol(Normalize(get)),output.Symbol(Normalize(baseGet)),"overrides",typeSyntax,dispatchKey:DispatchKey(baseGet));
                        if(property.SetMethod is { } set && baseProperty.SetMethod is { } baseSet) Edge(output.Symbol(Normalize(set)),output.Symbol(Normalize(baseSet)),"overrides",typeSyntax,dispatchKey:DispatchKey(baseSet));
                    }
            }
        }
        public void Reference(SimpleNameSyntax name, ISymbol symbol)
        {
            var kind=name.Ancestors().Any(n=>n is UsingDirectiveSyntax)?"imports":symbol is ITypeSymbol or INamespaceSymbol?"type_reference":"reference";
            Edge(Owner(name),output.Symbol(Normalize(symbol)),kind,name);
        }
        public void Operations(SyntaxNode node)
        {
            if (node is PrimaryConstructorBaseTypeSyntax primary && primary.Parent?.Parent is TypeDeclarationSyntax type
                && PrimaryConstructor(type) is { } constructor)
            {
                var owner = output.Symbol(Normalize(constructor));
                output.Occurrence(owner, source, type.Identifier.Span, "declaration", type.Span);
                var selected = model.GetSymbolInfo(primary).Symbol as IMethodSymbol;
                Edge(owner, selected is null ? null : output.Symbol(Normalize(selected)), "call", primary, dispatch: "direct");
                foreach (var argument in primary.ArgumentList.Arguments)
                    if ((model.GetOperation(argument) ?? model.GetOperation(argument.Expression)) is { } argumentOperation)
                        new Calls(this, owner).Visit(argumentOperation);
                return;
            }
            // Select executable roots, then walk each tree once. Nested function
            // operations update ownership inside the walker, not a second pass.
            SyntaxNode? root=node switch {
                BaseMethodDeclarationSyntax or AccessorDeclarationSyntax => node,
                EqualsValueClauseSyntax init when init.Parent is PropertyDeclarationSyntax || init.Parent is VariableDeclaratorSyntax { Parent.Parent: BaseFieldDeclarationSyntax } => node,
                PropertyDeclarationSyntax {ExpressionBody: { } body} => body.Expression,
                IndexerDeclarationSyntax {ExpressionBody: { } body} => body.Expression,
                GlobalStatementSyntax global => global.Statement,
                _ => null
            };
            if(root is not null && model.GetOperation(root) is { } operation)
            {
                var owner=Owner(node);
                if(node is AccessorDeclarationSyntax or PropertyDeclarationSyntax {ExpressionBody:not null} or IndexerDeclarationSyntax {ExpressionBody:not null})
                    output.Occurrence(owner,source,node.Span,"declaration",node.Span);
                new Calls(this,owner).Visit(operation);
            }
        }
        private sealed class Calls(GraphFacts graph, string owner) : OperationWalker
        {
            private string current=owner;
            private void Call(IMethodSymbol? method, IOperation operation, bool implicitCall=false, bool indirect=false)
            {
                var dispatch=indirect?"indirect":method?.ContainingType.TypeKind==TypeKind.Interface?"interface":
                    method is {IsStatic:false} && (method.IsVirtual||method.IsOverride||method.IsAbstract)?"virtual":"direct";
                if(operation.Syntax is InvocationExpressionSyntax {Expression:MemberAccessExpressionSyntax {Expression:BaseExpressionSyntax}}
                    or MemberAccessExpressionSyntax {Expression:BaseExpressionSyntax}
                    or ElementAccessExpressionSyntax {Expression:BaseExpressionSyntax})dispatch="direct";
                graph.Edge(current,indirect||method is null?null:graph.output.Symbol(Normalize(method)),"call",operation.Syntax,implicitCall||operation.IsImplicit,dispatch,graph.DispatchKey(method));
            }
            public override void VisitInvocation(IInvocationOperation operation)
            { Call(operation.TargetMethod,operation,indirect:operation.TargetMethod.MethodKind==MethodKind.DelegateInvoke);base.VisitInvocation(operation); }
            public override void VisitDynamicInvocation(IDynamicInvocationOperation operation)
            { Call(null,operation,indirect:true);base.VisitDynamicInvocation(operation); }
            public override void VisitInvalid(IInvalidOperation operation)
            {
                if(operation.Syntax is InvocationExpressionSyntax or ObjectCreationExpressionSyntax or ImplicitObjectCreationExpressionSyntax)
                    Call(null,operation,indirect:true);
                base.VisitInvalid(operation);
            }
            public override void VisitObjectCreation(IObjectCreationOperation operation)
            { Call(operation.Constructor,operation);base.VisitObjectCreation(operation); }
            public override void VisitPropertyReference(IPropertyReferenceOperation operation)
            {
                IOperation target=operation;
                while(target.Parent is { } parent && (parent is ITupleOperation or IParenthesizedOperation
                    || parent is IConversionOperation { IsImplicit:true })) target=parent;
                var assignment=target.Parent as IAssignmentOperation;
                var write=assignment?.Target==target;
                var both=write && (assignment is ICompoundAssignmentOperation or ICoalesceAssignmentOperation)
                    || target.Parent is IIncrementOrDecrementOperation increment && increment.Target==target;
                if((!write||both)&&operation.Property.GetMethod is { } get)Call(get,operation,true);
                if((write||both)&&operation.Property.SetMethod is { } set)Call(set,operation,true);
                base.VisitPropertyReference(operation);
            }
            public override void VisitBinaryOperator(IBinaryOperation operation)
            { if(operation.OperatorMethod is { } method)Call(method,operation,true);base.VisitBinaryOperator(operation); }
            public override void VisitUnaryOperator(IUnaryOperation operation)
            { if(operation.OperatorMethod is { } method)Call(method,operation,true);base.VisitUnaryOperator(operation); }
            public override void VisitConversion(IConversionOperation operation)
            { if(operation.OperatorMethod is { } method)Call(method,operation,true);base.VisitConversion(operation); }
            public override void VisitCompoundAssignment(ICompoundAssignmentOperation operation)
            { if(operation.OperatorMethod is { } method)Call(method,operation,true);base.VisitCompoundAssignment(operation); }
            public override void VisitIncrementOrDecrement(IIncrementOrDecrementOperation operation)
            { if(operation.OperatorMethod is { } method)Call(method,operation,true);base.VisitIncrementOrDecrement(operation); }
            public override void VisitAnonymousFunction(IAnonymousFunctionOperation operation)
            { var previous=current;current=graph.Lambda(operation);base.VisitAnonymousFunction(operation);current=previous; }
            public override void VisitLocalFunction(ILocalFunctionOperation operation)
            { var previous=current;current=graph.output.Symbol(Normalize(operation.Symbol));base.VisitLocalFunction(operation);current=previous; }
        }
    }
}
