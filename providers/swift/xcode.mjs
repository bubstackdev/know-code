import fs from 'node:fs';
import path from 'node:path';
import {send} from '../shared/protocol.mjs';
import {relative} from './index.mjs';

// The internal workspace belongs to the containing project, not another job.
export function canonicalXcodeProject(file) {
  if (path.basename(file) === 'contents.xcworkspacedata' && path.basename(path.dirname(file)) === 'project.xcworkspace'
      && path.dirname(path.dirname(file)).endsWith('.xcodeproj'))
    return path.join(path.dirname(path.dirname(file)), 'project.pbxproj');
  return file;
}

export async function analyzeXcode({request, root, project, toolchain}) {
  const projectPath = relative(root, project), id = `swift:${projectPath}`;
  const inputs = [project, ...toolchain.inputPaths];
  if (path.basename(project) === 'project.pbxproj') {
    const schemes = path.join(path.dirname(project), 'xcshareddata', 'xcschemes');
    if (fs.existsSync(schemes))
      inputs.push(...fs.readdirSync(schemes).filter(name => name.endsWith('.xcscheme')).map(name => path.join(schemes, name)));
  }
  send('compilation', {id, language: 'swift', projectPath, compilerVersion: toolchain.version,
    configuration: request.configuration, inputPaths: [...new Set(inputs)], optionsJson: JSON.stringify({
      origin: 'xcode', supported: false, compilerPath: toolchain.compiler,
      toolchainSelection: toolchain.selection, requiredPlatforms: ['iphoneos', 'iphonesimulator'],
      planner: 'reliable_configured_target_planner_required', preparation: 'not_run'
    })});
  send('report', {compilationId: id, status: 'partial', diagnostics: [{
    code: 'xcode_reliable_planner_required', severity: 'error', path: projectPath,
    message: 'Xcode project analysis needs a reliable planner that preserves device/simulator compilation contexts. '
      + 'The installed xcodebuild indexing-settings query can return simulator or x86_64 arguments despite an arm64 device request, '
      + 'so it cannot supply the required graph safely. A compatible configured-target BSP integration is not implemented yet. '
      + 'No project build, script, external generator, or standalone fallback was run. See docs/XCODE_PROVIDER.md.'
  }]});
}
