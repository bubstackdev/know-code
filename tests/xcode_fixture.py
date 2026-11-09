"""Create an owned Xcode fixture; never run its script phase or scheme action."""
import pathlib
import plistlib
import sys


def create(directory):
    root = pathlib.Path(directory).resolve()
    project = root / 'Probe.xcodeproj'
    schemes = project / 'xcshareddata' / 'xcschemes'
    schemes.mkdir(parents=True, exist_ok=True)
    (root / 'App.swift').write_text('''import Foundation
public struct DeviceContext {
  #if targetEnvironment(simulator)
  public static let simulatorOnly = true
  #else
  public static let deviceOnly = true
  #endif
  public static func message(_ value: String) -> String { value.uppercased() }
}
''')
    (root / 'UITests.swift').write_text('''import XCTest
final class ProbeUITests: XCTestCase {
  func testExample() { XCTAssertEqual("hello".uppercased(), "HELLO") }
}
''')
    objects = {}

    def obj(key, isa, **fields):
        objects[key] = dict(isa=isa, **fields)
        return key

    def configurations(prefix, settings):
        keys = [obj(prefix + name, 'XCBuildConfiguration', name=name,
                    buildSettings=settings) for name in ['Debug', 'Release']]
        return obj(prefix + 'Configurations', 'XCConfigurationList',
                   buildConfigurations=keys, defaultConfigurationName='Debug',
                   defaultConfigurationIsVisible='0')

    products = []
    targets = []
    files = []
    for name, source, product_type, extension in [
        ('Probe', 'App.swift', 'application', 'app'),
        ('ProbeUITests', 'UITests.swift', 'bundle.ui-testing', 'xctest'),
    ]:
        file = obj(name + 'Source', 'PBXFileReference', path=source,
                   lastKnownFileType='sourcecode.swift', sourceTree='<group>')
        files.append(file)
        buildfile = obj(name + 'BuildFile', 'PBXBuildFile', fileRef=file)
        sources = obj(name + 'Sources', 'PBXSourcesBuildPhase', files=[buildfile],
                      buildActionMask='2147483647', runOnlyForDeploymentPostprocessing='0')
        product = obj(name + 'Product', 'PBXFileReference', path=name + '.' + extension,
                      explicitFileType='wrapper.application' if extension == 'app' else 'wrapper.cfbundle',
                      sourceTree='BUILT_PRODUCTS_DIR')
        products.append(product)
        settings = {'PRODUCT_BUNDLE_IDENTIFIER': 'dev.knowcode.' + name,
                    'PRODUCT_NAME': '$(TARGET_NAME)', 'GENERATE_INFOPLIST_FILE': 'YES',
                    'CODE_SIGNING_ALLOWED': 'NO'}
        if extension == 'xctest':
            settings['TEST_TARGET_NAME'] = 'Probe'
        phases = [sources]
        if extension == 'app':
            phases.append(obj('NegativeScript', 'PBXShellScriptBuildPhase',
                buildActionMask='2147483647', files=[], inputPaths=[], outputPaths=[],
                runOnlyForDeploymentPostprocessing='0', shellPath='/bin/sh',
                shellScript='touch "$SRCROOT/PHASE_EXECUTED"', name='Must never execute'))
        target = obj(name + 'Target', 'PBXNativeTarget', name=name, productName=name,
                     buildConfigurationList=configurations(name, settings),
                     buildPhases=phases, buildRules=[], dependencies=[],
                     productReference=product, productType='com.apple.product-type.' + product_type)
        targets.append(target)
    products_group = obj('Products', 'PBXGroup', name='Products', children=products, sourceTree='<group>')
    main = obj('Main', 'PBXGroup', children=files + [products_group], sourceTree='<group>')
    settings = {'SDKROOT': 'iphoneos', 'SWIFT_VERSION': '6.0',
                'IPHONEOS_DEPLOYMENT_TARGET': '27.0', 'SUPPORTED_PLATFORMS': 'iphoneos iphonesimulator',
                'SWIFT_OPTIMIZATION_LEVEL': '-Onone'}
    obj('Project', 'PBXProject', buildConfigurationList=configurations('Project', settings),
        mainGroup=main, productRefGroup=products_group, projectDirPath='', projectRoot='',
        targets=targets, compatibilityVersion='Xcode 15.0', developmentRegion='en', knownRegions=['en'])
    with (project / 'project.pbxproj').open('wb') as output:
        plistlib.dump({'archiveVersion': '1', 'objectVersion': '60', 'classes': {},
                      'objects': objects, 'rootObject': 'Project'}, output)
    entries = ''.join(f'<BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="YES" buildForArchiving="YES" buildForAnalyzing="YES"><BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="{name}Target" BuildableName="{name}.{ext}" BlueprintName="{name}" ReferencedContainer="container:Probe.xcodeproj"/></BuildActionEntry>'
                      for name, ext in [('Probe', 'app'), ('ProbeUITests', 'xctest')])
    (schemes / 'Probe.xcscheme').write_text(f'''<?xml version="1.0" encoding="UTF-8"?>
<Scheme LastUpgradeVersion="2700" version="1.3">
<BuildAction parallelizeBuildables="NO" buildImplicitDependencies="YES"><PreActions><ExecutionAction ActionType="Xcode.IDEStandardExecutionActionsCore.ExecutionActionType.ShellScriptAction"><ActionContent title="Must never execute" scriptText="touch &quot;$SRCROOT/SCHEME_EXECUTED&quot;"/></ExecutionAction></PreActions><BuildActionEntries>{entries}</BuildActionEntries></BuildAction>
<TestAction buildConfiguration="Debug" selectedDebuggerIdentifier="Xcode.DebuggerFoundation.Debugger.LLDB" selectedLauncherIdentifier="Xcode.IDEFoundation.Launcher.LLDB" shouldUseLaunchSchemeArgsEnv="YES"><Testables><TestableReference skipped="NO"><BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="ProbeUITestsTarget" BuildableName="ProbeUITests.xctest" BlueprintName="ProbeUITests" ReferencedContainer="container:Probe.xcodeproj"/></TestableReference></Testables></TestAction>
<LaunchAction buildConfiguration="Debug"/><ProfileAction buildConfiguration="Release"/><AnalyzeAction buildConfiguration="Debug"/><ArchiveAction buildConfiguration="Release"/>
</Scheme>''')
    return project


if __name__ == '__main__':
    print(create(sys.argv[1]))
