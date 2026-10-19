# Eval reference answers

Hand-checked references for the 20 questions in [`questions.toml`](questions.toml), written 2026-10-12. Status:
**approved by owner 2026-10-12** (the M0 exit criterion).

How to read each entry:
- **Q** is the question exactly as the agent sees it.
- **A** is the short answer, with `path:line` items.
- **Checked** says how the answer was verified.
- **Grading** says how a run is scored.

Every answer was verified at the pinned commit with `git grep` and `git show`, never against a working tree, since
several working trees have moved or are dirty. Line numbers refer to the pin. KnowCode was used at most to find
candidates and never as the source of truth.

Questions marked "Revised" were reworded during checking so they have one checkable answer.

| id | category | repo | question |
|---|---|---|---|
| [find-01](#find-01) | find | bubs-maps | Where is the type that models a saved place defined, and what are its stored properties? Give file:line for each. |
| [find-02](#find-02) | find | aether-engine | List every type (class, struct, enum, actor, protocol) under Sources/ whose name ends in "Engine", with file:line. |
| [find-03](#find-03) | find | workspace | Which SwiftUI `App` types are marked `@main` across the workspace? Give repo and file:line. |
| [context-01](#context-01) | context | face-hugger | What does PairingArchive do? Show its definition, its methods, and every call site of its load() method. |
| [context-02](#context-02) | context | renaml | Summarize RenameRecipe: its stored properties, initializer, conformances and extensions, and the main functions in Sources/ that produce or consume it. |
| [context-03](#context-03) | context | bubs-mail | Where is MailMessage defined, what module and conformances does it have, is it extended anywhere, and which SwiftUI views take one? |
| [trace-01](#trace-01) | trace | bubstack | Which symbols create a BubstackCard, directly or through the BubsCard typealias, and what reaches them within three hops? |
| [trace-02](#trace-02) | trace | midi-me | Starting from MIDIPlaybackController.play(), list the in-repo methods it calls, two levels deep. Ignore AVFoundation and MediaPlayer calls. |
| [trace-03](#trace-03) | trace | midi-me | Who calls MIDIPlaybackController.loadSoundBank(url:), directly or within three hops? Only app code counts; standalone test harnesses are listed separately. |
| [trace-04](#trace-04) | trace | idk-play | Which types conform to YouTubeDurationProviding, and where is the protocol used and called? |
| [impact-01](#impact-01) | impact | bubs-maps | If `BubRouteModel.retry()` (app/Bub/Route/BubRouteModel.swift:304) gains a required parameter, which call sites stop compiling (Critical) and which functions reach them one hop further (High)? |
| [impact-02](#impact-02) | impact | face-hugger | `QueueArchive` saves `[UploadJob]` as JSON in queue.json. If that on-disk format changes, which types define it, and which code and tests read or write it? |
| [impact-03](#impact-03) | impact | renaml | With `eval/patches/impact-03.diff` applied, which changes `RecipeLibrary.remove(id:)` into `remove(ids: Set<UUID>)`, which symbols are affected within 2 hops? |
| [hotspots-01](#hotspots-01) | hotspots | aether-engine | Which five hand-written Swift files (excluding `records/` and `*/Generated/`) had the most lines changed in the 90 days up to the pin commit, ranked? |
| [hotspots-02](#hotspots-02) | hotspots | bubs-maps | Among Swift files touched by at least 5 commits in the year up to the pin, which have the highest share of commits whose subject matches `\b(bug/revert/hotfix)\b`? |
| [overview-01](#overview-01) | overview | midi-me | Orient me in midi-me: list its build targets and entry points, and name the main model and view types and the file where playback lives. |
| [overview-02](#overview-02) | overview | space-case | In space-case's FactoryCore, where are Cell, Tile and Factory defined, and how do they relate? |
| [cross-repo-01](#cross-repo-01) | cross-repo | workspace | Which repos depend on the bubstack package, through which manifest, and which products do they use? |
| [cross-repo-02](#cross-repo-02) | cross-repo | workspace | Which bubs-mail files import Bubstack modules, and which BubstackKit symbols does each use? |
| [cross-repo-03](#cross-repo-03) | cross-repo | workspace | If the spacing tokens on `BubstackTheme.Spacing` (`theme.space.*`) are renamed, which files in which repos break? |

<a id="find-01"></a>

## find-01 · find · bubs-maps @ a462d943bd05
**Q:** Where is the type that models a saved place defined, and what are its stored properties? Give file:line for each.
**A:** `struct BubPlace` at `app/Bub/Places/BubPlaces.swift:9`, with 8 stored properties (`id` is computed, not stored).
- `app/Bub/Places/BubPlaces.swift:9` struct BubPlace: Identifiable, Equatable, Sendable
- `app/Bub/Places/BubPlaces.swift:15` uid: String
- `app/Bub/Places/BubPlaces.swift:16` title: String
- `app/Bub/Places/BubPlaces.swift:17` address: String
- `app/Bub/Places/BubPlaces.swift:18` coordinate: CLLocationCoordinate2D
- `app/Bub/Places/BubPlaces.swift:19` role: Role?
- `app/Bub/Places/BubPlaces.swift:21` savedAt: Date
- `app/Bub/Places/BubPlaces.swift:23` isRough: Bool
- `app/Bub/Places/BubPlaces.swift:25` placeId: UInt64 = 0
**Checked:** `git grep -n -E "(struct|class|enum|actor) BubPlace" a462d943bd05 -- '*.swift'` found the type, then I read `git show a462d943bd05:app/Bub/Places/BubPlaces.swift` lines 1–34. Not counted: the nested `enum Role` (line 10) and the computed `id` (line 27). `BubPlaces` (line 37) is the store, not the model.
**Grading:** set (items above)

<a id="find-02"></a>

## find-02 · find · aether-engine @ c9eeb4299473
**Q:** List every type (class, struct, enum, actor, protocol) under Sources/ whose name ends in "Engine", with file:line.
**A:** 8 types; 7 are top level, plus one private nested enum.
- `Sources/AetherRuntime/Engine.swift:105` protocol Engine: Actor
- `Sources/AetherRuntime/ContextEngine.swift:13` actor ContextEngine: Engine
- `Sources/AetherRuntime/ExpertStreamEngine.swift:30` actor ExpertStreamEngine: Engine (`@_spi(ExpertStreaming)`)
- `Sources/AetherRuntime/FunctionEngine.swift:10` enum FunctionEngine
- `Sources/AetherRuntime/ImageEncoderEngine.swift:17` actor ImageEncoderEngine
- `Sources/AetherGrammar/GrammarEngine.swift:16` final class GrammarEngine: Sendable
- `Sources/AetherTesting/AetherTesting.swift:819` actor MockEngine: Engine
- `Sources/AetherText/ChatTemplateRenderer.swift:49` private enum Engine (nested in ChatTemplateRenderer)
**Checked:** `git grep -n -E "(struct|class|enum|actor|protocol|typealias) [A-Za-z_]*Engine([^A-Za-z_]|$)" c9eeb4299473`, filtered to `Sources/`. `records/` (archived copies) and `docs/` are excluded by the question's scope. Revised: the original said "type" without listing kinds; it now names the kinds and Sources/ explicitly.
**Grading:** set (items above). The nested private `Engine` counts, but missing it costs recall only, not precision.

<a id="find-03"></a>

## find-03 · find · workspace @ per-repo pins (aether-engine c9eeb4299473, bubs-mail 5f1fcb2f10aa, bubs-maps a462d943bd05, bubstack 946d2f115b4a, face-hugger cac29bac7822, idk-play ea256e572f4f, midi-me 31875748b728, renaml 261313586bd2, space-case 1eaedb9b0f04)
**Q:** Which SwiftUI `App` types are marked `@main` across the workspace? Give repo and file:line.
**A:** 12 `@main` SwiftUI App types: 4 in aether-engine and one in each of the other 8 Swift repos.
- aether-engine `apps/BenchHost/App/BenchHostApp.swift:17` BenchHostApp
- aether-engine `apps/DeviceHarness/App/HarnessApp.swift:14` HarnessApp
- aether-engine `apps/Showcase/App/ShowcaseApp.swift:9` ShowcaseApp
- aether-engine `spikes/P0/iOS/App/P0App.swift:6` P0App
- bubs-mail `App/BubsMailApp.swift:6` BubsMailApp
- bubs-maps `app/Bub/App/BubApp.swift:6` BubApp
- bubstack `Examples/BubstackDemo/Sources/BubstackDemoApp.swift:9` BubstackDemoApp
- face-hugger `Sources/FaceHugger/FaceHuggerApp.swift:39` FaceHuggerApp
- idk-play `IDKPlay/App/IDKPlayApp.swift:5` IDKPlayApp
- midi-me `App/MIDIMeApp.swift:4` MIDIMeApp
- renaml `Sources/RenaMLApp/RenaMLApp.swift:5` RenaMLApp
- space-case `App/SpaceCaseApp.swift:2` SpaceCaseApp
**Checked:** `git -C <repo> grep -n "@main" <pin> -- '*.swift'` in each repo, reading the declaration after each hit with `git show <pin>:<file>`. Cross-checked with `git grep -E "struct [A-Za-z0-9_]+: (SwiftUI\.)?App( |\{|,)" <pin>`; the only extra hits were Markdown snippets (README, `.superdesign`, `docs/`). The line given is the `struct` line; when `@main` is on its own line, it's the line above. Excluded: aether-engine `records/` (archived copies), and non-App `@main` executables (CLI tools, probes and smoke tests, e.g. renaml `Sources/renaml/RenaMLCLI.swift:6`, face-hugger `Scripts/SandboxProbe/main.swift:176`, midi-me `App/Audio/Tests/*`, and aether-engine `spikes/expert-streaming/*` and `scripts/*`). Revised: the original asked for "app entry points (@main)", which matched about 60 CLI and probe mains in aether-engine; it's now limited to SwiftUI `App` types.
**Grading:** set (items above, keyed by repo + type name)

<a id="context-01"></a>

## context-01 · context · face-hugger @ cac29bac7822
**Q:** What does PairingArchive do? Show its definition, its methods, and every call site of its load() method.
**A:** `PairingArchive` (`Sources/FaceHuggerCore/Pairing.swift:20`) stores `[SavedPairing]` as JSON at a URL. `load()` returns `[]` when the file is missing, and `save` creates the parent directory and writes atomically. `load()` is called at 6 sites.
- `Sources/FaceHuggerCore/Pairing.swift:20` struct PairingArchive: Sendable
- `Sources/FaceHuggerCore/Pairing.swift:21` let url: URL
- `Sources/FaceHuggerCore/Pairing.swift:22` init(url:)
- `Sources/FaceHuggerCore/Pairing.swift:23` func load() throws -> [SavedPairing]
- `Sources/FaceHuggerCore/Pairing.swift:27` func save(_ pairings: [SavedPairing]) throws
- load() call: `Sources/FaceHugger/AppModel.swift:61` (AppModel.init, via `pairingArchive`, declared at line 39)
- load() calls: `Tests/FaceHuggerCoreTests/PairingTests.swift:29`, `:31`, `:34` (archive from line 9)
- load() calls: `Tests/FaceHuggerCoreTests/PairingTests.swift:41`, `:53` (`PairingArchive(url:).load()`)
**Checked:** read `git show cac29bac7822:Sources/FaceHuggerCore/Pairing.swift` (lines 20–31; it has no extensions, since `git grep "extension PairingArchive"` is empty). `git grep -n -E "\.load\(" cac29bac7822 -- '*.swift'` gives 10 hits. I kept those whose receiver is a PairingArchive: `pairingArchive` is bound at AppModel.swift:39, and `archive` is bound at PairingTests.swift:9 in the same test function. Excluded as not PairingArchive: `AppModel.swift:62` and `QueueTests.swift:100` (QueueArchive), and `DatasetCardSheet.swift:49,165` (DatasetCardWorkspace).
**Grading:** set (the 5 definition items plus the 6 load() call sites; the excluded sites count as false positives)

<a id="context-02"></a>

## context-02 · context · renaml @ 261313586bd2
**Q:** Summarize RenameRecipe: its stored properties, initializer, conformances and extensions, and the main functions in Sources/ that produce or consume it.
**A:** `RenameRecipe` (`Sources/RenaMLCore/Models.swift:17`) is a plain value type: an instruction string plus an ordered list of `RenameOperation`s. It has no methods and no extensions. It's produced by the instruction, example and planner front ends, and consumed by the rename engine, the recipe library and the workspace model.
- `Sources/RenaMLCore/Models.swift:17` struct RenameRecipe: Codable, Sendable, Equatable
- `Sources/RenaMLCore/Models.swift:18` var instruction: String
- `Sources/RenaMLCore/Models.swift:19` var operations: [RenameOperation]
- `Sources/RenaMLCore/Models.swift:20` init(instruction: String = "", operations:)
- produces: `Sources/RenaMLCore/InstructionInterpreter.swift:7` InstructionInterpreter.interpret(_:)
- produces: `Sources/RenaMLCore/ExampleInference.swift:16` ExampleInference.infer(examples:)
- produces: `Sources/RenaMLIntelligence/LocalPlanner.swift:37` LocalPlanner.recipe(instruction:filenames:)
- consumes: `Sources/RenaMLCore/RenameEngine.swift:47` RenameEngine.preview(urls:recipe:progress:) -> RenamePlan
- consumes: `Sources/RenaMLCore/RenameEngine.swift:14` RenameEngine.proposedName(for:recipe:index:dateContext:)
- consumes: `Sources/RenaMLCore/RecipeLibrary.swift:73` RecipeLibrary.validate(recipe:), `:46` RecipeLibrary.add(name:recipe:)
- consumes: `Sources/RenaMLApp/WorkspaceModel.swift:213` WorkspaceModel.useRecipe(_:source:)
- stored in: `Sources/RenaMLCore/Models.swift:49` RenamePlan.recipe, `Sources/RenaMLCore/RecipeLibrary.swift:6` SavedRecipe.recipe
**Checked:** read `git show 261313586bd2:Sources/RenaMLCore/Models.swift` (lines 17–23). `git grep -n -E "(struct|class|enum|actor) RenameRecipe|extension RenameRecipe" 261313586bd2` finds only the definition. `git grep -n -E "func [A-Za-z_]+\([^)]*RenameRecipe|-> RenameRecipe" 261313586bd2 -- 'Sources/*.swift'` and `git grep "RenameRecipe\("` gave the producers and consumers. I read each enclosing type's declaration to qualify the names. Revised: the original asked for methods and extensions, of which there are none. It now asks for producers and consumers too, so the answer has substance.
**Grading:** rubric: (1) defined at Models.swift:17 with exactly two stored properties, instruction and operations; (2) conforms to Codable, Sendable, Equatable, with no methods and no extensions; (3) names at least two producers (InstructionInterpreter.interpret, ExampleInference.infer, LocalPlanner.recipe); (4) names RenameEngine.preview as the main consumer, producing RenamePlan; (5) mentions RecipeLibrary save/validate or WorkspaceModel.useRecipe; (6) every location given is a correct file:line.

<a id="context-03"></a>

## context-03 · context · bubs-mail @ 5f1fcb2f10aa
**Q:** Where is MailMessage defined, what module and conformances does it have, is it extended anywhere, and which SwiftUI views take one?
**A:** `public struct MailMessage: Codable, Equatable, Sendable, Identifiable` lives in the MailCore module and has no extensions; four App views take one. Revised: the original asked "which modules extend it and which views read it"; there are no extensions, so the question now asks whether there are any.
- `Sources/MailCore/MailModels.swift:36`: the definition, with 17 stored properties (id, threadID, accountID, sender, address, subject, body, sentAt, isUnread, mailbox, attachments, providerID, providerLabels, providerFolderID, plainTextAvailable, html, bodyTextSource) and an init at :57
- Extensions: none at the pin
- `App/MessageReaderViews.swift:7` MessageEnvelope (`let message: MailMessage` :8)
- `App/MessageReaderViews.swift:53` FoldedMessageRow (`let message` :54)
- `App/MessageReaderViews.swift:103` FormattedMessageBody (`let message` :104)
- `App/ThreadView.swift:6` ThreadView, through `messageView(_:in:)` :133, `handle(_:linkDomains:from:)` :184 and `refuse(_:)` :197
**Checked:** `git grep -n -w MailMessage 5f1fcb2f10aa -- '*.swift'` (with a per-file count). Grepped `extension MailMessage`: no hits. Read MailModels.swift:36–64 and the App struct headers with `git show`.
**Grading:** rubric: (1) defined at MailModels.swift:36 in MailCore; (2) the four conformances; (3) no extensions; (4) the four views above with file:line; (5) bonus: it is grouped into MailThread (MailModels.swift:84).

<a id="trace-01"></a>

## trace-01 · trace · bubstack @ 946d2f115b4a
**Q:** Which symbols create a BubstackCard, directly or through the BubsCard typealias, and what reaches them within three hops?
**A:** Only one site creates a BubstackCard: `CardSample.body`. It is reached through `GalleryRow.core`, then `GalleryRow.all`, then `BubstackGallery.body`. The `BubsCard` typealias is declared but never used in this repo.
- hop 0 (definition): `Sources/BubstackKit/Components/BubstackCard.swift:27` `public struct BubstackCard`, plus its convenience extension at :112
- hop 1: `Sources/BubstackGallery/GalleryRows.swift:171` `CardSample.body` calls `BubstackCard(face:title:subtitle:subtitleSpoken:onClose:)`
- hop 2: `Sources/BubstackGallery/GalleryRows.swift:44` and `:45` `GalleryRow.core` (declared :15) creates `CardSample()`
- hop 3: `Sources/BubstackGallery/GalleryRows.swift:12` `GalleryRow.all = core + nav`
- hop 4: `Sources/BubstackGallery/BubstackGallery.swift:56` `BubstackGallery.body` reads `GalleryRow.all`. This is beyond 3 hops; don't penalize it.
- typealias: `Examples/BubAdoption/BubsCompat.swift:150` `typealias BubsCard = BubstackCard`, with no uses (the only other hit is a comment at :2)
**Checked:** `git grep -n -w -E "BubstackCard|BubsCard" 946d2f115b4a`, `git grep -n -w CardSample`, `git grep GalleryRow.all`. Read GalleryRows.swift:9–46 and :165–173 with `git show`.
**Grading:** set: hop-1 to hop-3 items (171, 44/45, 12) plus the typealias line; hop 4 is optional.

<a id="trace-02"></a>

## trace-02 · trace · midi-me @ 31875748b728
**Q:** Starting from MIDIPlaybackController.play(), list the in-repo methods it calls, two levels deep. Ignore AVFoundation and MediaPlayer calls.
**A:** `play()` (`App/Audio/MIDIPlaybackController.swift:273`) calls 7 in-repo methods directly. Those fan out to `allNotesOff`, `send`, `updatePosition`, `register`, `displayTitle`, and `seek`/`play`/`pause`/`stop` through remote-command closures. All paths below are in `App/Audio/MIDIPlaybackController.swift`.
- hop 1: `:280` `startupRecovery.begin()` and `:301` `startupRecovery.cancel()`, on PlaybackStartupRecovery (begin :42, cancel :47; the property is at :119)
- hop 1: `:285` `seek(to:)` (def :376), then hop 2: `allNotesOff` :382, `applyChannelOverrides` :385, `updateNowPlaying` :386, `startupRecovery.cancel` :380
- hop 1: `:289` `applyChannelOverrides()` (def :506), then hop 2: `send(_:)` :508, :511–513
- hop 1: `:296` `configureRemoteCommands()` (def :643), then hop 2: `register(_:action:)` :645–648; `play`/`pause`/`stop` inside closures :645–649; `seek(to:)` :655; `updateNowPlaying` :658
- hop 1: `:298` `observePosition()` (def :581), then hop 2: `updatePosition()` :587, inside a Task closure
- hop 1: `:299` and `:305` `updateNowPlaying()` (def :673), then hop 2: `Self.displayTitle(_:)` :687
**Checked:** read play() at :273–307 and each callee's body at the pin with `git show 31875748b728:App/Audio/MIDIPlaybackController.swift`. midi-me's HEAD has moved past the pin, so line numbers refer to the pin only.
**Grading:** set: the 7 hop-1 callees are required (begin, cancel, seek, applyChannelOverrides, configureRemoteCommands, observePosition, updateNowPlaying). Hop 2 is scored on allNotesOff, send, updatePosition, register and displayTitle. The closure-dispatched play/pause/stop calls are optional.

<a id="trace-03"></a>

## trace-03 · trace · midi-me @ 31875748b728
**Q:** Who calls MIDIPlaybackController.loadSoundBank(url:), directly or within three hops? Only app code counts; standalone test harnesses are listed separately.
**A:** Four app sites call `loadSoundBank` directly: the controller's own `init` and three in PlayerStore. Through `importSoundBank` and `useBundledSoundBank`, these reach `PlayerRootView`, `MIDIMeApp.makeStore` and `PlayerStore.playback`.
- definition: `App/Audio/MIDIPlaybackController.swift:236`
- hop 1: `App/Audio/MIDIPlaybackController.swift:148` in `MIDIPlaybackController.init(engine:)` (:124), which loads the default bank
- hop 1: `App/Model/PlayerStore.swift:78` in `PlayerStore.init(directory:defaults:…)` (:43), which restores the saved bank
- hop 1: `App/Model/PlayerStore.swift:406` in `PlayerStore.importSoundBank(_:)` (:393)
- hop 1: `App/Model/PlayerStore.swift:422` in `PlayerStore.useBundledSoundBank()` (:418)
- hop 2: `App/Model/PlayerStore.swift:18` `PlayerStore.playback = MIDIPlaybackController()`, which runs the init
- hop 2: `App/UI/PlayerRootView.swift:56` and `:91` in `PlayerRootView.body` (:42), which call importSoundBank
- hop 2: `App/UI/PlayerRootView.swift:229` in `PlayerRootView.optionItems` (:217), which calls useBundledSoundBank
- hop 2: `App/MIDIMeApp.swift:21` and `:24` in `MIDIMeApp.makeStore()` (:16), which construct PlayerStore and so run its init
- hop 3: `App/UI/PlayerRootView.swift:194` in `PlayerRootView.header(compact:)` (:184), via `Menu { optionItems }`; `App/MIDIMeApp.swift:5` `MIDIMeApp.store = MIDIMeApp.makeStore()`
- test harnesses, direct: `App/Audio/Tests/AudioFeatures.swift:126`, `AudioIntegration.swift:42` (in `RealtimeRig.init`), `:134`, `:137`, `AudioPitch.swift:73`, `AudioSmoke.swift:51`; via importSoundBank and useBundledSoundBank: `App/Model/Tests/PlayerStoreSmoke.swift:123–145`; via PlayerStore.init: `Scripts/RenderPlayer.swift:23`
**Checked:** `git grep -n -w loadSoundBank 31875748b728`, then `git grep` for importSoundBank, useBundledSoundBank, `PlayerStore(`, `MIDIPlaybackController(` and optionItems. Enclosing symbols were found by reading `git show` output upward from each line. midi-me's HEAD has moved, so line numbers refer to the pin only.
**Grading:** set: the 4 hop-1 sites are required; hop 2 is scored on PlayerStore.swift:18, PlayerRootView.swift:56/91/229 and MIDIMeApp.swift:21/24. Hop 3 and the test harnesses are optional, but listing harness sites as app callers is not an error.

<a id="trace-04"></a>

## trace-04 · trace · idk-play @ ea256e572f4f
**Q:** Which types conform to YouTubeDurationProviding, and where is the protocol used and called?
**A:** One type conforms: `YouTubeDurationFetcher`. The protocol is used only in the share extension's `ShareImportModel`, as an injected provider that defaults to the fetcher, and is called once from `beginDurationLookup()`.
- protocol: `Shared/Import/YouTubeDurationProviding.swift:3`, with its requirement `durationSeconds(for:)` at :4
- conformer: `Shared/Import/YouTubeDurationFetcher.swift:3` `struct YouTubeDurationFetcher: YouTubeDurationProviding`, whose witness is at :12
- use: `ShareExtension/App/ShareImportModel.swift:16` the stored property `durationProvider: any YouTubeDurationProviding`
- use: `ShareExtension/App/ShareImportModel.swift:26` an init parameter that defaults to `YouTubeDurationFetcher()`
- call: `ShareExtension/App/ShareImportModel.swift:44` `durationProvider.durationSeconds(for:)` in `ShareImportModel.beginDurationLookup()` (:41)
- construction: `ShareExtension/App/ShareViewController.swift:21` in `presentConfirmation(for:)`, which takes the default provider (no injection)
- no test doubles conform; tests call only the static `YouTubeDurationFetcher.parseDuration(from:)` (`IDKPlayTests/Share/YouTubeDurationParserTests.swift:11`, `:20`)
**Checked:** `git grep -n -w YouTubeDurationProviding ea256e572f4f` (4 hits; no extension conformances). Also grepped durationProvider, YouTubeDurationFetcher and `ShareImportModel(`. Read the protocol file and ShareImportModel.swift:1–46 with `git show`.
**Grading:** set: the protocol, the single conformer, and the uses at :16, :26 and :44 are required; the construction site and the test note are optional. Revised: the question now says "where is the protocol used and called", because each conformance has only one use site.

<a id="impact-01"></a>

## impact-01 · impact · bubs-maps @ a462d943bd05
**Q:** If `BubRouteModel.retry()` (app/Bub/Route/BubRouteModel.swift:304) gains a required parameter, which call sites stop compiling (Critical) and which functions reach them one hop further (High)?
**A:** 4 call sites in 3 files break; 4 more functions are one hop away. All are in `app/`, and no other repo is affected.
- Critical: `BubRouteCardView.perform`, at `app/Bub/Route/BubRouteCard.swift:113` (`case .tryAgain: route.retry()`)
- Critical: `BubRouteModel.waitForFix`, at `app/Bub/Route/BubRouteModel.swift:574` (`self.retry()` in the fix-poll task)
- Critical: `BubRouteModel.mapsChanged`, at `app/Bub/Route/BubRouteModel.swift:587`
- Critical: `BubRouteModelTests.testInternalErrorThenStuck`, at `app/BubTests/BubRouteModelTests.swift:326`
- High: `BubRouteCardView.button`, at `app/Bub/Route/BubRouteCard.swift:79` (calls `perform`)
- High: `BubRouteModel.run`, at `app/Bub/Route/BubRouteModel.swift:425` (calls `waitForFix`)
- High: `BubRouteModel.noPosition`, at `app/Bub/Route/BubRouteModel.swift:560` (calls `waitForFix`)
- High: `BubRouteModel.start`, at `app/Bub/Route/BubRouteModel.swift:266` (a notification observer calls `mapsChanged`)
**Checked:**
- Ran `git grep -n "retry()\|\.retry(" a462d943bd05 -- '*.swift'`. The only definition is BubRouteModel.swift:304. The hit at BubPackHost.swift:4 is a comment and is excluded.
- Found the enclosing functions by reading the files with `git show a462d943bd05:<path>`, and the hop-2 sites with `git grep -n "waitForFix(\|mapsChanged()\|perform("`.
- `BubPackHost.mapsChanged` (BubPackHost.swift:54) is a different method that shares the name, so it is excluded.
- `SpikeRootView.waitForFix` is a different static func, so it is excluded.
**Grading:** set. Score Critical and High separately on `path:line`.
Revised: the question now names the definition and asks for exactly hops 1 and 2. The original asked to "classify by hop distance" with no depth.

<a id="impact-02"></a>

## impact-02 · impact · face-hugger @ cac29bac7822
**Q:** `QueueArchive` saves `[UploadJob]` as JSON in queue.json. If that on-disk format changes, which types define it, and which code and tests read or write it?
**A:** The format is `UploadJob`'s synthesized `Codable`, which nests `HubRepo`, `RepoKind` and `JobState`. Three things read or write it: `AppModel` (which loads at init and saves in `persist()`), a smoke script, and three tests.
- Defines the format: `QueueArchive` at `Sources/FaceHuggerCore/Models.swift:74`, with `load` at `:77` and `save` at `:85`
- Defines the format: `UploadJob: Codable` at `Sources/FaceHuggerCore/Models.swift:40`, with `JobState` at `:19`, `HubRepo` at `:8` and `RepoKind` at `:3`
- Critical: `AppModel.archive` at `Sources/FaceHugger/AppModel.swift:54`, `AppModel.init` loading it at `:62`, and `AppModel.persist` saving it at `:65`
- Critical: the smoke script decodes `UploadJob` from a fixture at `Scripts/NativeSmoke/main.swift:54`
- Test: `QueueTests.testInterruptedJobIsRecoveredWithoutRestarting` at `Tests/FaceHuggerCoreTests/QueueTests.swift:93`, which does a save and load at `:99–100`
- Test: `FolderAccessTests.testBookmarkFieldsRoundTripAndLegacyArchivesRemainReadable` at `Tests/FaceHuggerCoreTests/FolderAccessTests.swift:54`, which round-trips and decodes a legacy job at `:58` and `:64`
- High: the 10 `persist()` calls in `AppModel.swift` and 1 in `TransferShelf.swift`. Every queue mutation saves through it.
**Checked:**
- Ran `git grep -n "QueueArchive\|UploadJob.self\|archive\.\(load\|save\)" cac29bac7822 -- '*.swift'`.
- Read `Models.swift` at the pin with `git show cac29bac7822:Sources/FaceHuggerCore/Models.swift`, and counted `persist()` with `git grep -c`.
- The archive calls in `PairingTests` go to `PairingArchive`, not `QueueArchive`, so they are excluded.
**Grading:** set. Required items: the 2 defining types, the 3 AppModel sites, the smoke script and the 2 tests. The persist() count is a bonus.
Revised: the question now says what the "on-disk format" is (`[UploadJob]` JSON) and asks for the defining types plus the code and tests that read or write it. "Blast radius" alone was unscorable.

<a id="impact-03"></a>

## impact-03 · impact · renaml @ 261313586bd2
**Q:** With `eval/patches/impact-03.diff` applied, which changes `RecipeLibrary.remove(id:)` into `remove(ids: Set<UUID>)`, which symbols are affected within 2 hops?
**A:** The changed symbol is `RecipeLibrary.remove`, at `Sources/RenaMLCore/RecipeLibrary.swift:61`. Three callers break (4 call sites), and one view property is one hop further.
- Changed: `RecipeLibrary.remove(id:)` → `remove(ids:)`, at `Sources/RenaMLCore/RecipeLibrary.swift:61`
- Critical (hop 1): `WorkspaceModel.removeRecipe`, at `Sources/RenaMLApp/WorkspaceModel.swift:250`
- Critical (hop 1): `RecipeLibraryTests.testPersistsAcrossInstancesAndDeletesOnlyRequestedRecipe`, at `Tests/RenaMLCoreTests/RecipeLibraryTests.swift:21` and `:23`
- Critical (hop 1): `RecipeLibraryTests.testCorruptLibraryNeverSilentlyOverwritten`, at `Tests/RenaMLCoreTests/RecipeLibraryTests.swift:43`
- High (hop 2): `WorkspaceView.recipeMenu`, at `Sources/RenaMLApp/WorkspaceView.swift:130` (the Delete button calls `model.removeRecipe(saved)`)
**Checked:**
- Built the patch from `git show 261313586bd2:Sources/RenaMLCore/RecipeLibrary.swift`. It applies cleanly (`git apply --check` in renaml, whose copy of that file equals the pin).
- Found the call sites with `git grep -n "\.remove(id:" 261313586bd2 -- '*.swift'`, and their enclosing functions by reading the files at the pin.
- Hop 2: `git grep -n "removeRecipe"` gives one hit, at WorkspaceView.swift:130, inside `private var recipeMenu: some View` (line 115).
- No other repo depends on renaml.
**Grading:** set. Required: the changed symbol, the 3 hop-1 symbols, and the 1 hop-2 symbol.

<a id="hotspots-01"></a>

## hotspots-01 · hotspots · aether-engine @ c9eeb4299473
**Q:** Which five hand-written Swift files (excluding `records/` and `*/Generated/`) had the most lines changed in the 90 days up to the pin commit, ranked?
**A:** In rank order: `Publisher.swift`, `AetherTesting.swift`, `ImageGenerator.swift`, `RecipeValidation.swift` and `Converter.swift`. The window covers the whole history, since the repo starts on 2026-09-22.
- 1. `Sources/AetherForgeKit/Publisher.swift`: 1,365 lines changed, 30 commits, complexity 262 over 39 functions
- 2. `Sources/AetherTesting/AetherTesting.swift`: 1,174 lines, 32 commits, complexity 217 over 84 functions
- 3. `Sources/AetherGen/ImageGenerator.swift`: 963 lines, 20 commits, complexity 144 over 32 functions
- 4. `Sources/AetherForgeKit/RecipeValidation.swift`: 962 lines, 50 commits, complexity 374 over 15 functions (the most complex and most often changed)
- 5. `Sources/AetherForgeKit/Converter.swift`: 905 lines, 18 commits, complexity 148 over 18 functions
- Just outside the top 5: `Sources/AetherRuntime/FunctionEngine.swift`, 793 lines
**Checked:**
- The pin's date is 2026-10-09T09:25:58-06:00, so the window starts at 2026-07-11T09:25:58-06:00.
- Ran `git log c9eeb4299473 --since=2026-07-11T09:25:58-06:00 --no-merges --numstat --format= -- '*.swift' ':!records/'` and summed added plus deleted lines per file.
- Excluded `Sources/AetherStore/Generated/BundledCatalog.swift` (7,354 lines, generated). Commit counts come from `git log <pin> --oneline -- <file>`.
- Complexity is KnowCode's per-function branch count, summed per file and read from the index. These files are identical to the pin in the working tree (`git diff --quiet <pin> -- <file>`).
**Grading:** set. The top 5 files are required, and rank order earns partial credit. Lines changed must be within ±5%. Complexity figures are informational only, since a baseline agent can't reproduce them exactly.
Revised: "relative to their complexity" was undefined. The question now ranks by 90-day lines changed, with an explicit window and exclusions, and reports complexity as context.

<a id="hotspots-02"></a>

## hotspots-02 · hotspots · bubs-maps @ a462d943bd05
**Q:** Among Swift files touched by at least 5 commits in the year up to the pin, which have the highest share of commits whose subject matches `\b(bug|revert|hotfix)\b`?
**A:** None. No commit that touches a Swift file matches the pattern, so all 14 qualifying files have a share of 0. A correct answer says so and names no hotspot.
- 14 files qualify (5 or more commits); for example `app/Bub/Debug/BubsTestState.swift`, `app/Bub/Spike/SpikeRootView.swift` and `app/BubUITests/BubRouteUITests.swift` (13 commits each), all at 0 of 13.
- Only 2 of the 385 commits up to the pin match at all: `abc1257` ("…check/apply/revert") and `3d58e9c` ("…traced to a real bug…"). They touch only `scripts/redflips_neverred.py` and `docs/`, no Swift files.
**Checked:**
- The pin's date is 2026-10-12T14:36:40-06:00, and the window starts at 2025-10-12. The repo's history begins on 2026-09-07, so the window covers the whole history.
- Ran `git log a462d943bd05 --since=2025-10-12T14:36:40-06:00 --no-merges --format='@@%s' --name-only -- '*.swift'`. A Python counter then tallied, per file, the matching commits (case-insensitive) against all commits, keeping files with 5 or more commits.
- Cross-checked with `git log a462d943bd05 --no-merges --format='%h %s' | grep -iE '\b(bug|revert|hotfix)\b'` (2 hits) and `git show --stat` on each.
**Grading:** exact. Pass if the answer says no file has a nonzero share; fail if it names any file as a bug-fix hotspot.
Revised (owner, 2026-10-12): dropped `fix|fixes|fixed` from the pattern, because "fix" in bubs-maps usually means a GPS fix. With the narrower pattern, this question tests that the tool doesn't invent hotspots.

<a id="overview-01"></a>

## overview-01 · overview · midi-me @ 31875748b728
**Q:** Orient me in midi-me: list its build targets and entry points, and name the main model and view types and the file where playback lives.
**A:** An XcodeGen app with Mac and iOS targets over a local SwiftPM package called MIDIKit. `MIDIMeApp` creates a `PlayerStore`, which owns a `MIDIPlaybackController`, and the UI root is `PlayerRootView`.
- `project.yml:20`: target `MIDIMeMac` (macOS app). `project.yml:71`: `MIDIMeiOS` (iOS app). `project.yml:124` and `:135`: the macOS and iOS UI-test bundles.
- `project.yml:17`: local package `MIDIKit` at `path: .`. Both apps use the products `MIDIKit` and `MIDILibrary`.
- `Package.swift:8-10`: products `MIDIKit` and `MIDILibrary` (libraries) and `midi-inspect` (executable). Test targets: `MIDIKitTests` and `MIDILibraryTests`.
- `App/MIDIMeApp.swift:3-4`: `@main struct MIDIMeApp: App`, the app entry point.
- `Sources/MIDIInspect/main.swift`: CLI entry point (`midi-inspect`).
- `App/Model/PlayerStore.swift:7`: `final class PlayerStore: ObservableObject`, the app model; created at `App/MIDIMeApp.swift:24`.
- `App/UI/PlayerRootView.swift:9`: `struct PlayerRootView: View`, the root view; created at `App/MIDIMeApp.swift:29`.
- `App/Audio/MIDIPlaybackController.swift:76`: `final class MIDIPlaybackController`, where playback lives (`play()` at `:273`). It is owned by `PlayerStore.playback` at `App/Model/PlayerStore.swift:18`.
- `Sources/MIDIKit/MIDIFile.swift:78`: `MIDIFile`, the parsed-file model. `Sources/MIDILibrary/MIDILibrary.swift:6`: `MIDILibrary`, the song library.
**Checked:** At the pin, ran `git show 31875748b728:project.yml`, `git show 31875748b728:Package.swift` and `git ls-tree -r`, plus `git grep -n` for type declarations, `@main` and constructor sites. HEAD has moved to 40ce49942b92; every line number is from the pin.
**Grading:** rubric. Key points:
1. Names both app targets (macOS and iOS), built with XcodeGen.
2. Names the local SwiftPM package and its MIDIKit and MIDILibrary libraries.
3. Names `MIDIMeApp` (`@main`) as the entry point; mentioning `midi-inspect` is a bonus.
4. Names `PlayerStore` as the model and `PlayerRootView` as the root view.
5. Places playback in `App/Audio/MIDIPlaybackController.swift`, owned by `PlayerStore`.

<a id="overview-02"></a>

## overview-02 · overview · space-case @ 1eaedb9b0f04
**Q:** In space-case's FactoryCore, where are Cell, Tile and Factory defined, and how do they relate?
**A:** All three are value types in `Sources/FactoryCore/Factory.swift`. A `Factory` holds `tiles: TileMap<Tile>`, a grid keyed by `Cell` coordinates. `Tile` is one placed `Building` with its direction and in-flight item state.
- `Sources/FactoryCore/Factory.swift:36`: `public struct Cell`, an integer grid coordinate (`x`, `y`); `next(_ direction:)` steps one cell.
- `Sources/FactoryCore/Factory.swift:61`: `public struct Tile`, which holds `kind: Building`, `direction`, `item`, `progress` and an optional `identity: UUID`.
- `Sources/FactoryCore/Factory.swift:109`: `public struct Factory`, the whole simulation state: tiles, credits, science and per-world states (lunar, cinder and others).
- `Sources/FactoryCore/Factory.swift:112`: `public var tiles: TileMap<Tile>`, the relation Factory → Tile, keyed by Cell.
- `Sources/FactoryCore/TileMap.swift:67`: `public struct TileMap<Value>`; `subscript(cell: Cell)` at `:92` is the Cell → Tile lookup.
- `Sources/FactoryCore/Factory.swift:205` (`place(_:at:facing:)`), `:209` (`remove(at:)`) and `:256` (`tick(_:)`): Factory's mutators that take Cells, and the simulation step.
**Checked:** At the pin, ran `git grep -n -P "(class|struct|...) (Cell|Tile|Factory)\b"` (the only other `Tile` is a script-local struct in `Scripts/Art/contact_sheet.swift:14`, excluded) and read `git show 1eaedb9b0f04:Sources/FactoryCore/Factory.swift` lines 1–150 and its method list. The line numbers for `tiles`, `place`, `remove` and `tick` were confirmed with `git grep -n` at the pin.
**Grading:** rubric. Key points:
1. All three are structs in `Sources/FactoryCore/Factory.swift`.
2. Cell is an x/y grid coordinate.
3. Tile is a placed Building plus its direction and item state.
4. Factory owns `tiles: TileMap<Tile>`, keyed by Cell.
5. Factory's `tick`, `place` and `remove` drive the simulation.

<a id="cross-repo-01"></a>

## cross-repo-01 · cross-repo · workspace @ per-repo pins (see Checked)
**Q:** Which repos depend on the bubstack package, through which manifest, and which products do they use?
**A:** Only one other repo does: bubs-mail, through its XcodeGen `project.yml`, by local path, using the product `BubstackKit`. bubstack's own demo app is the only other consumer.
- `bubs-mail/project.yml:7-8`: package `Bubstack` at `path: ../bubstack`.
- `bubs-mail/project.yml:17-18`: target `BubsMail` depends on `package: Bubstack`, `product: BubstackKit`.
- Internal consumer, not a separate repo: `bubstack/Examples/BubstackDemo/project.yml:8` (`path: ../..`), using `BubstackKit` (`:17-18`) and `BubstackNav` (`:19-20`).
- No dependency in aether-engine, bubs-maps, face-hugger, idk-play, midi-me, renaml, space-case, bubfinder, lars-sdk, bubstack.dev, bubstack-lab or knowcode. No `Package.swift`, `project.yml`, `Cargo.toml` or `.pbxproj` in them references `../bubstack` or `bubstackdev/bubstack`. The other `bubstack` hits are URLs and e-mail addresses (renaml `project.yml:37-39`) and the repo URL in lars-sdk `Cargo.toml:9`.
**Checked:** For every repo, listed the manifests with `git ls-tree -r --name-only <pin> | grep -E "(Package.swift|project.yml|Cargo.toml)$"` and ran `git show <pin>:<manifest> | grep -i bubstack`. Then ran `git grep -E "\.\./bubstack|bubstackdev/bubstack"` over `*.pbxproj`, `*.yml`, `Package.swift`, `Package.resolved` and `*.toml`. Pins: aether-engine c9eeb4299473, bubs-mail 5f1fcb2f10aa, bubs-maps a462d943bd05, bubstack 946d2f115b4a, face-hugger cac29bac7822, idk-play ea256e572f4f, midi-me 31875748b728, renaml 261313586bd2, space-case 1eaedb9b0f04, bubfinder 82b514e515bb, lars-sdk e1ed4f82b4de, bubstack.dev 3b843be80279, bubstack-lab 7c5d4f274eef, knowcode 137e301e35a9 (all four at HEAD on 2026-10-12).
**Grading:** set. Required: (bubs-mail, `project.yml`, `../bubstack`, `BubstackKit`). The BubstackDemo entry is optional credit. Penalize any other repo listed as a dependent.

<a id="cross-repo-02"></a>

## cross-repo-02 · cross-repo · workspace @ bubs-mail 5f1fcb2f10aa, bubstack 946d2f115b4a
**Q:** Which bubs-mail files import Bubstack modules, and which BubstackKit symbols does each use?
**A:** Eight files in `App/` import BubstackKit, and no other Bubstack module. All eight read `\.bubstackTheme`. The symbols used, with the first-use line in each file:
- `App/BubsMailApp.swift:3`: `bubstackTheme`:22, `BubstackTheme`:23
- `App/ComposerView.swift:3`: `bubstackTheme`:14, `BubstackChip`:31, `bubstackPrimary`:39, `bubstackSecondary`:40, `bubstackSheetBackground`:59, `BubstackSheetSection`:94, `BubstackRowButtonStyle`:104, `bubstackSheetRowDivider`:105
- `App/InboxView.swift:3`: `bubstackTheme`:16, `BubstackChip`:41, `BubstackMenuButton`:110, `bubstackSecondary`:201, `bubstackSecondaryCompact`:218
- `App/MailChrome.swift:3`: `bubstackTheme`:10, `BubstackKeyStyle`:19, `bubstackFullWidthButtons`:20, `BubstackSearchBar`:26, `bubstackGlass`:26, `BubstackCloseButton`:42, `bubstackIcon`:62, `BubstackChip`:99
- `App/MessageHTMLView.swift:6`: `BubstackSwatch`:8 (extended), `BubstackAppearance`:10, `BubstackTheme`:18
- `App/MessageReaderViews.swift:4`: `bubstackTheme`:10, `BubstackChip`:133, `bubstackLoading`:168, `BubstackAppearance`:185, `bubstackSecondaryCompact`:262, `BubstackButtonRow`:282, `bubstackSecondary`:283, `bubstackPrimary`:284, `bubstackSheetBackground`:307
- `App/SettingsView.swift:3`: `bubstackTheme`:15, `BubstackSheetSection`:25, `BubstackValueRow`:29, `BubstackValueText`:30, `BubstackRowButtonStyle`:36, `BubstackChip`:43, `bubstackLoading`:50, `bubstackSecondary`:50, `bubstackFullWidthButtons`:51, `bubstackSheetBackground`:100
- `App/ThreadView.swift:3`: `bubstackTheme`:23, `bubstackSecondaryCompact`:44, `bubstackGlass`:51, `bubstackSecondary`:79, `BubstackButtonRow`:95, `bubstackSheetBackground`:129, `bubstackIcon`:155, `BubstackChip`:159, `bubstackPrimary`:224
**Checked:**
- Imports: `git -C bubs-mail grep -n "^import Bubstack" 5f1fcb2f10aa`.
- Symbol list: BubstackKit's public type names (68) and its public `bubstack*` funcs and vars (22), via `git -C bubstack grep -o -P "public ... <Name>" 946d2f115b4a -- Sources/BubstackKit`.
- Matching: whole-word matches in each file at the pin, skipping comment lines. Dropped `State`, which collides with SwiftUI's `@State`.
- Spot-checked six sites by reading them: MailChrome:99, ThreadView:155, BubsMailApp:23, InboxView:110, MessageHTMLView:8, SettingsView:29.
- Caveat: theme members reached through `theme.` (for example `theme.space.gapMedium`) are counted under `bubstackTheme`, not listed one by one.
**Grading:** set, over (file, symbol) pairs. The 8 files are required; credit symbol pairs by precision and recall, ignoring line numbers.

<a id="cross-repo-03"></a>

## cross-repo-03 · cross-repo · workspace @ bubstack 946d2f115b4a, bubs-mail 5f1fcb2f10aa (others at their pins)
**Q:** If the spacing tokens on `BubstackTheme.Spacing` (`theme.space.*`) are renamed, which files in which repos break?
Revised: the question now names the `Spacing` struct, so "spacing tokens" is unambiguous.
**A:** They're defined in `bubstack/Sources/BubstackKit/Theme/BubstackTheme.swift:258` (`Spacing`, 15 `CGFloat` tokens) and reached through `BubstackTheme.space` (`:14`). Renaming them breaks 12 more files in bubstack (including one test file) and 7 in bubs-mail; no other repo uses them.
- bubstack, 12 files (first use):
  - `Sources/BubstackKit/Theme/BubstackTokens.swift:73` (`s = primary.space`, then `s.cardPadding` at `:114`)
  - `Sources/BubstackKit/Components/BubstackSheet.swift:62` (28 uses)
  - `Sources/BubstackKit/Components/BubstackCard.swift:57`
  - `Sources/BubstackKit/Components/BubstackKeyStyle.swift:189`
  - `Sources/BubstackKit/Components/BubstackKnob.swift:165`
  - `Sources/BubstackKit/Components/BubstackSearchBar.swift:44`
  - `Sources/BubstackKit/Components/BubstackToken.swift:86`
  - `Sources/BubstackNav/BubstackGuidanceCards.swift:178`
  - `Sources/BubstackNav/BubstackGuidanceParts.swift:148`
  - `Sources/BubstackGallery/GalleryRows.swift:163`
  - `Examples/BubAdoption/BubsCompat.swift:141`
  - `Tests/BubstackKitTests/BubstackMetricsTests.swift:78`
- bubs-mail, 7 files:
  - `App/InboxView.swift:39` (28 uses)
  - `App/MessageReaderViews.swift:14` (10)
  - `App/ThreadView.swift:31` (8)
  - `App/ComposerView.swift:21` (6)
  - `App/MailChrome.swift:16`
  - `App/SettingsView.swift:20`
  - `App/MessageHTMLView.swift:23`
**Checked:** Read `Spacing` at the bubstack pin. In every Swift repo at its pin, ran `git grep -c -P "\.space\.(cardPadding|sheetPadding|stackGap|margin|marginBottom|rowHorizontal|rowVertical|rowGap|keyGap|keyPadding|keyPaddingCompact|gapTight|gapSmall|gapMedium|fieldInset)\b"`, then `git grep -n -m1` for the first-use lines. A separate search for indirect access (`= x.space`) found only `BubstackTokens.swift:73`. bubs-maps, face-hugger, idk-play, midi-me, renaml, space-case and aether-engine have no matches; none of them depends on bubstack (see cross-repo-01).
**Grading:** set, over files. The 19 files above; repo attribution is required.
