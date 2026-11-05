### KnowCode review of `shop-api` (the working tree against `4b825dc642cb`)

changed files: 2 · consumers broken: 1, possibly broken: 1 · rule matches: 1 · tests to run: 1 · dependent files: 2

**Not whole**: see the sections marked cut, and what the index can't settle.

#### Contracts this change breaks

- http `DELETE /baskets/{id}` removed at `src/BasketController.cs:29` <!-- kc:7777777777777777 -->
  - **broken**: `BasketClient.cancel` at `shop-web/src/api/basket.ts:21` [exact] <!-- kc:8888888888888888 -->
  - possibly broken: `BasketService.drop` at `shop-admin/src/basket.service.ts:7` [unique\_name] <!-- kc:9999999999999999 -->

_Not whole (unsupported)._

#### Rule matches

- warning `no-result` at `src/Checkout.cs:41-42`: blocking on &lt;Task&gt;.Result <!-- kc:aaaaaaaaaaaaaaaa -->

#### What changed

- `src/BasketController.cs` modified: `BasketController.Submit`, `BasketController.Withdraw` (removed) and 2 more <!-- kc:1111111111111111 -->
- `src/Ledger|Book.cs` renamed from `src/Ledger.cs` <!-- kc:2222222222222222 -->

#### What depends on it

- `src/Checkout.cs` <!-- kc:5555555555555555 -->
  - line 40: `Checkout.Run` → `BasketController.Submit` (calls, direct [exact])
  - and 2 more
- `shop-web/src/api/basket.ts` <!-- kc:6666666666666666 -->
  - line 8: `BasketClient.place` (http, direct [exact])

_2 of 5 shown (capped); more: `kc {op: review, diff: working, dependents: 10}`._

#### Tests to run

_Advice: a test not listed may still fail._

- `tests/BasketTests.cs` (0.82): `dotnet test tests/Shop.Tests.csproj --filter BasketTests` <!-- kc:3333333333333333 -->

#### Files that usually change with these, left alone

- `src/BasketDto.cs`: with `src/BasketController.cs` in 5 of its commits <!-- kc:4444444444444444 -->

#### What the index can't settle

- policy: which of several implementations calls reach: edges left as ambiguous weren't walked past
