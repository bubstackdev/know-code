# compare-v1's fresh repositories: how they were chosen

Step 5 of 0.8's additions (README.md) asks Go, Java and Kotlin questions on repositories nobody tuned on: none of
identity-v1's or identity-v2's, nor any another evaluation or workstream has picked or is about to pick. They are
chosen by a rule written before any tool was asked about them, and every candidate passed over is listed with why.

The search files here (search-*.tsv) keep each result's rank, full name, stars and licence; the outcome for each is in
the tables below. The searches as saved, with GitHub's free-text descriptions, are kept outside the repository. Their
SHA-256 digests are in README.md (step 5), so this record can be checked against them.

## The rule (the lead's, 2026-10-29)

Candidates come from GitHub's search on 2026-10-29, `language:L stars:>3000 size:3000..40000 archived:false
fork:false`, sorted by stars, taken in that order:

1. A library or framework (code other projects import), not an application, a collection of samples or tutorials,
   or a tool.
2. A permissive licence: MIT, Apache-2.0 or BSD.
3. Built by the language's own toolchain with no platform SDK: Go modules, Maven or Gradle (no Android).
4. Not used by any evaluation or workstream: identity-v1 and identity-v2 (with their held-out sets), route-links-v1,
   tasks-v1, history-v1, public-v1 to v3, impact-dev-v1, find-dev-v1, echo, jsoup, jackson-core; W2's Kotlin held-outs
   (ktlint, Exposed, Netflix/dgs-framework); W1's routes-v1 repositories and its routes held-outs. Added after the
   picks (2026-10-30), with no outcome changed:
   - W2's later Kotlin held-outs, ZacSweers/MoshiX and cashapp/misk, which are in no search read here. dgs-framework
     stays excluded, although W2 has since dropped it.
   - W1's routes-v1.2 held-outs, flipped-aurora/gin-vue-admin and smallstep/certificates, and the candidates it read
     and dropped: hashicorp/nomad, golang/website, nats-io/nats-server, raeperd/realworld.go, owncast/owncast,
     marmotdata/marmot and buggregator/server. Only gin-vue-admin is in a search read here, at Go's rank 45, below both
     Go picks.
5. The first two per language that label cleanly in the pinned containers ([run.sh](../../../scripts/eval/identity/run.sh)
   with this directory's [repos.txt](repos.txt) and [builds.txt](builds.txt)). A mixed Java and Kotlin repository
   counts toward Kotlin only if Kotlin is at least half of its references.

Each JVM candidate gets scip-java's default build as identity-v2 uses it (Maven `clean verify` without tests on JDK
17; Gradle `scipCompileAll` on JDK 21). When that fails in a way one of the identity labeller's documented opt-ins
addresses, it gets one more attempt with that opt-in, and the opt-in is listed here: `SCIP_SKIP_BUILDSRC=1`
([skip-buildsrc.gradle](../../../scripts/eval/identity/jvm/skip-buildsrc.gradle), for scip-java's Kotlin plugin in a
build's own logic) and git-commit-id-maven-plugin's `-Dmaven.gitcommitid.skip=true` (run.sh copies the tracked files,
without `.git`), the JDK choice builds.txt offers (17 or 21), `KOTLIN_CATALOG` or a repository's own Kotlin version
setting (only for a Kotlin-version compiler error), and a plugin's own documented skip for a build step that compiles
nothing (maven-gpg-plugin's `-Dgpg.skip=true`, docker-maven-plugin's `-Ddocker.skip=true`) (the lead, 2026-10-29). A
failure no opt-in addresses is a skip. A repository that ships a server application beside its
library (seata, Sentinel) is ambiguous under rule 1 and is skipped (the lead, 2026-10-29).

**Chosen:** Go: charmbracelet/bubbletea, go-gorm/gorm. Java: binarywang/WxJava, brettwooldridge/HikariCP. Kotlin:
google/ksp, oshai/kotlin-logging.

## Java

| Rank | Repository | Outcome |
| ---: | --- | --- |
| 1–5 | doocs/advanced-java, TheAlgorithms/Java, kdn251/interviews, termux/termux-app, skylot/jadx | Rule 1: tutorials, collections, an application, a tool |
| 6 | lysine-dev/retrofit | Rule 3: its `:retrofit:android-test` module needs the Android SDK; the build fails |
| 7 | xkcoding/spring-boot-demo | Rule 1: demos |
| 8 | binarywang/WxJava | **Labels cleanly** (BUILD SUCCESS; 446 of its 4,886 source files aren't in the index) |
| 9–12 | xuxueli/xxl-job, alibaba/canal, lenve/vhr, ashishps1/awesome-low-level-design | Rules 1 and 2: GPL, an application, no licence, a collection |
| 13 | apache/incubator-seata | Rule 1: ships a server application beside its library, so it's ambiguous and is skipped (it labelled cleanly) |
| 14–17 | JakeWharton/butterknife, hollischuang/toBeTopJavaer, LSPosed/LSPosed, Netflix/Hystrix | Rules 2–3: Android, no licence, GPL, no licence |
| 18 | redisson/redisson | The default build fails: its Quarkus module needs JDK 21. On JDK 21 (the opt-in) `redisson-micronaut-50` fails to compile, which no opt-in addresses |
| 19 | google/gson | Rule 4: identity-v2 |
| 20 | alibaba/Sentinel | Rule 1: ships a server application (`sentinel-dashboard`) beside its library, as seata does |
| 21 | elunez/eladmin | Rule 1: an application |
| 22 | brettwooldridge/HikariCP | **Labels cleanly** with `-Ddocker.skip=true` (the default build's `verify` ran docker-maven-plugin, which needs a Docker daemon); all 114 source files are in the index |
| 23 | linlinjava/litemall | Rule 1: an application |
| 24 | dromara/Sa-Token | Labels cleanly with `-Dgpg.skip=true` (the default build's `verify` signed the artifacts), but HikariCP ranks above it |
| 25 | Baseflow/PhotoView | Rule 3: Android |
| 26 | LMAX-Exchange/disruptor | Labels cleanly (106 of its 240 source files, outside the compiled source sets, aren't in the index), but HikariCP ranks above it |

Ranks are those of [search-java.tsv](search-java.tsv), saved when the rule was applied.

## Kotlin

Ranks are those of [search-kotlin.tsv](search-kotlin.tsv): pages 1–2 saved when the rule was first applied, pages 3–4
(ranks 61 on) about an hour later, as the scan reached them, so their star counts are a little later.

| Rank | Repository | Outcome |
| ---: | --- | --- |
| 1–20 | android/architecture-samples, gkd-kit/gkd, PhilJay/MPAndroidChart, RikkaApps/Shizuku, alibaba/p3c, JunkFood02/Seal, google-ai-edge/gallery, MatsuriDayo/NekoBoxForAndroid, tiann/KernelSU, amir1376/ab-download-manager, square/picasso, iamgio/quarkdown, nickbutcher/plaid, JingMatrix/Vector, shihabal3amri/DiPlay, YiiGuxing/TranslationPlugin, mikepenz/MaterialDrawer, square/moshi, zhanghai/MaterialFiles, firebase/quickstart-android | Rules 1–4: samples, applications, GPL or no licence, an IDE plugin (TranslationPlugin), coding guidelines with an IDE plugin (p3c), Android libraries (MPAndroidChart, picasso, MaterialDrawer), and square/moshi (identity-v2) |
| 21 | lysine-dev/okio | The default build fails: the multiplatform module `:okio-nodefilesystem` has no JVM compile task; no opt-in addresses it |
| 22 | javalin/javalin | With git-commit-id's skip (the default build failed for want of `.git`) it labels, but 0 of its 2,781 references are Kotlin: its Kotlin isn't indexed under Maven, so it isn't Kotlin by rule 5 |
| 23–39 | skydoves/Pokedex, bmax121/APatch, uber/RIBs, skydoves/android-developer-roadmap, pengchujin/aerial-tv, ReadYouApp/ReadYou, LibChecker/LibChecker, wxxsfxyzm/InstallerX-Revived, romanvht/ByeByeDPI, aistra0528/Hail, igorwojda/android-showcase, sourcerer-io/sourcerer-app, Mygod/VPNHotspot, ACRA/acra, meta/facebook-android-sdk, samolego/Canta, z-huang/InnerTune | Rules 1–3: applications, collections, GPL or LGPL, Android frameworks and SDKs (RIBs, ACRA, the Facebook SDK) |
| 40 | Kotlin/kotlinx.serialization | Fails in `:build-settings-logic` (`freeCompilerArgs` queried before its accessors exist), with and without `SCIP_SKIP_BUILDSRC=1`, which acts only after that point |
| 41–42 | airbnb/mavericks, fcitx5-android/fcitx5-android | Rules 2–3: Android, LGPL |
| 43 | mockk/mockk | Fails in `:buildSrc` the same way, with and without `SCIP_SKIP_BUILDSRC=1` |
| 44–61 | drakeet/MultiType, kizitonwose/Calendar, Dr-TSNG/Hide-My-Applist, dovecoteescapee/ByeDPIAndroid, Tapadoo/Alerter, hxh19950701/WebViewTvLive, mikepenz/Android-Iconics, didi/booster, react-native-async-storage/async-storage, VREMSoftwareDevelopment/WiFiAnalyzer, orhanobut/dialogplus, agrosner/DBFlow, osfans/trime, izhangzhihao/intellij-rainbow-brackets, moezbhatti/qksms, ChuckerTeam/chucker, phodal/auto-dev, orailnoor/DroidDesk | Rules 1–3: Android libraries and applications, React Native, a build optimizer for Android, MPL |
| 62 | embabel/embabel-agent | Default build: git-commit-id needs `.git`; with its skip, kapt fails to compile `embabel-agent-ai`, which no opt-in addresses |
| 63–65 | jing332/tts-server-android, burhanrashid52/PhotoEditor, mikepenz/AboutLibraries | Rules 1–3: an Android application, Android libraries, a Gradle plugin with Android targets (AboutLibraries) |
| 66 | square/wire | The default build fails: the multiplatform module `:samples:native` has no JVM compile task, as okio; no opt-in addresses it. (First passed over by mistake: a filter dropped every description naming Android. Found when every candidate was written out here, and attempted in its place.) |
| 67–99 | Fractal-Compute-Orchestrations/FractalAndroid, airbnb/DeepLinkDispatch, zly2006/zhihu-plus-plus, fumiama/copymanga, Triple-T/gradle-play-publisher, GradleUp/shadow, AhmadHassan-BTed/AhSilence, square/kotlinpoet, ssseasonnn/RxDownload, KieronQuinn/TapTap, iamr0s/Dhizuku, rumboalla/apkupdater, skydoves/Balloon, AhmadHassan-BTed/MotionShot, aaa1115910/bv, mikepenz/FastAdapter, skydoves/chatgpt-android, RookieEnough/Orion-Store, TermoraDev/termora, xororz/local-dream, Yalantis/Context-Menu.Android, liujingxing/rxhttp, frknkrc44/HMA-OSS, JetBrains/intellij-platform-plugin-template, git-xuhao/KotlinMvp, ExTV/Podroid, mpv-android/mpv-android, brahmkshatriya/echo, KieronQuinn/Smartspacer, etchdroid/etchdroid, connectbot/connectbot, princekin-f/EasyFloat, Kotlin/kotlin-lsp | Rules 1–4: Android libraries and applications, Gradle and IDE plugins and templates, square/kotlinpoet (identity-v2), a language server |
| 100 | google/ksp | **Labels cleanly** with `SCIP_SKIP_BUILDSRC=1` (the default build failed with an internal compiler error in `:buildSrc:compileKotlin`); 97% of its references are Kotlin; 443 of its 811 `.kt` and `.java` files aren't in the index |
| 101 | MobileNativeFoundation/Store | Fails in an included build's `:tooling:plugins:compileKotlin`, not `buildSrc`, so no opt-in addresses it |
| 102 | Netflix/dgs-framework | Rule 4: W2's Kotlin held-out |
| 103 | DanielMartinus/Konfetti | Rule 3: Android |
| 104 | kosi-libs/Kodein | Rule 3: its build needs an Android module (`:framework:android:…`) |
| 105–114 | wgtunnel/android, kuafuai/andee, patrykandpatrick/vico, whyorean/AuroraStore, damontecres/Wholphin, wuseal/JsonToKotlinClass, NexAlloy/NexAlloy, adrielcafe/voyager, pedroSG94/RootEncoder, mahlernim/google-timeline-visualizer | Rules 1–3: applications, Compose Multiplatform UI libraries |
| 115 | ajalt/clikt | The default build: an internal compiler error in `:clikt:compileKotlinJvm` (Kotlin 2.3.0 against scip-java's plugin for 2.4). With `KOTLIN_CATALOG=2.4.21` (the opt-in): Kotlin/Native's "Unknown host target: linux aarch64", which no opt-in addresses. This one depends on the host: the containers run on arm64 |

### The extended Kotlin search

The search above has only 115 Kotlin results, and it gave one clean Kotlin pick (google/ksp). The lead extended it on
2026-10-30, before any further candidate was labelled: the same query with the size floor lowered from 3,000 KB to
500 KB (`language:kotlin stars:>3000 size:500..40000 archived:false fork:false`, by stars). Many Kotlin JVM libraries
are smaller than 3,000 KB. The extension's results are saved in [search-kotlin-ext.tsv](search-kotlin-ext.tsv).
Candidates already seen in the first search are passed over, the rest are taken in that order under the same rules and
opt-ins, and the scan stops after 60 new candidates. If none of those gives a second clean pick, Kotlin is published
with ksp alone, and the results say so.

| Rank (extended) | Repository | Outcome |
| ---: | --- | --- |
| 14–35 | orhanobut/logger, JakeWharton/timber, android/skills, chrisbanes/cheesesquare, ArisGuimera/Android-Expert, zetbaitsu/Compressor | Rules 1–3: Android libraries, samples, a course, no licence |
| 39 | ReactiveX/RxKotlin | The default build's Gradle daemon won't start on JDK 21; its wrapper is Gradle 5.4.1, too old for JDK 17 as well, so no opt-in addresses it |
| 47–65 | capcom6/android-sms-gateway, diogobernardino/williamchart, android10/Android-CleanArchitecture-Kotlin | Rules 1–3: an Android application, an unlicensed Android library, a sample |
| 68 | kittinunf/fuel | Fails creating `:plugins:compileKotlin` in an included build, not `buildSrc`; no opt-in addresses it |
| 69–134 | ReVanced/revanced-patches-template, ChadCSong/ShineButton, sharish/ShimmerRecyclerView, orhanobut/hawk, tanujnotes/Olauncher, RedApparat/Fotoapparat, gurecn/YuyanIme, TakuSemba/Spotlight, guolindev/PermissionX, tommybuonomo/dotsindicator, ReVanced/revanced-patcher, LSPosed/CorePatch, oblador/react-native-keychain, raamcosta/compose-destinations, zacharee/InstallWithOptions, YenalyLiew/Han1meViewer, Tinder/Scarlet | Rules 1–3: Android libraries and applications, GPL, no licence, React Native, an input method, a deprecated application |
| 139 | mockito/mockito-kotlin | Fails creating `:build-logic:compileKotlin` in an included build; no opt-in addresses it |
| 141 | oshai/kotlin-logging | **Labels cleanly** with `KOTLIN_CATALOG=2.4.21` (the default build failed to load scip-java's Kotlin plugin under Kotlin 2.1.21); all its references are Kotlin; 45 of its 101 source files, in its non-JVM source sets, aren't in the index |

The extended search's other new candidate, kyujin-cho/pixel-volte-patch (143, GPL), isn't reached. 31 new candidates, under
the cap of 60.

## Go

Rule 4's Go exclusions come from kc-w1-go (2026-10-29): identity-v2's cobra, chi, gin and echo; routes-v1's
repositories; its proposed routes held-outs (golang-gin-realworld-example-app, golang-echo-realworld-example-app,
goatcounter, miniflux, go-vikunja/vikunja, golang/pkgsite), remark42, go-base, go-gin-example, gitea, and the sftpgo pair.
Also find-dev-v1's Go repositories (kc-w4-rules): spf13/pflag and urfave/cli. Ranks are those of
[search-go.tsv](search-go.tsv).

| Rank | Repository | Outcome |
| ---: | --- | --- |
| 1–3 | avelino/awesome-go, JuliusBrussee/caveman, fatedier/frp | Rule 1: a list, a tool, an application |
| 4 | gin-gonic/gin | Rule 4: identity-v2 |
| 5–9 | junegunn/fzf, caddyserver/caddy, nektos/act, wagoodman/dive, jesseduffield/lazydocker | Rule 1: tools and a server application |
| 10 | AlistGo/alist | Rule 2: AGPL |
| 11 | charmbracelet/bubbletea | **Labels cleanly** |
| 12–15 | astaxie/build-web-application-with-golang, XTLS/Xray-core, schollz/croc, evanw/esbuild | Rules 1–2: a book, MPL, an application, a tool |
| 16 | go-gorm/gorm | **Labels cleanly** |
