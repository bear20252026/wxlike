/// 绑定未生成时的占位实现：每个调用都返回同一类失败。
///
/// 它存在的意义不是"让页面跑起来"，而是让页面**诚实地**显示阻塞原因。任何
/// 返回空列表或示例数据的兜底都会把"没接线"伪装成"没有内容"，这是本组明确
/// 禁止的。
///
/// 异步方法返回失败的 `Future` 而不是同步 `throw`：页面普遍在 `late Future`
/// 字段里直接调用端口，同步抛出会绕过 `AsyncView` 的三态，直接把异常炸在 build
/// 阶段；失败 Future 才会进入页面级失败面板。
library;

import 'core.dart';
import 'models.dart';
import 'ports.dart';

G13Failure _unwired(String port) => G13Failure.upstreamNotWired(port);

Future<T> _fail<T>(String port) => Future<T>.error(_unwired(port));

class UnavailableFormatPort implements FormatPort {
  const UnavailableFormatPort();

  @override
  String money(Money value) => throw _unwired('FormatPort.money');
}

class UnavailableContentPort implements ContentPort {
  const UnavailableContentPort();

  @override
  Future<PageResult<Moment>> moments({String cursor = '', int limit = 20}) =>
      _fail('content.moments');

  @override
  Future<Moment> publishMoment(
    MomentDraft draft, {
    required String idempotencyKey,
  }) => _fail('content.publish');

  @override
  Future<Moment> setMomentLike(
    String momentId,
    bool liked, {
    required String idempotencyKey,
  }) => _fail('content.like');

  @override
  Future<MomentComment> commentMoment(
    String momentId,
    String text, {
    required String idempotencyKey,
  }) => _fail('content.comment');

  @override
  Future<void> deleteMoment(
    String momentId, {
    required String idempotencyKey,
  }) => _fail('content.delete');

  @override
  Future<PageResult<FavoriteItem>> favorites({
    String cursor = '',
    String type = '',
    int limit = 20,
  }) => _fail('favorite.list');

  @override
  Future<FavoriteItem> addFavorite({
    required String type,
    required String sourceId,
    required String note,
    required String idempotencyKey,
  }) => _fail('favorite.add');

  @override
  Future<void> removeFavorite(
    String favoriteId, {
    required String idempotencyKey,
  }) => _fail('favorite.remove');

  @override
  Future<PageResult<StickerPack>> stickerPacks({
    String cursor = '',
    int limit = 20,
  }) => _fail('sticker.list');

  @override
  Future<StickerPack> downloadStickerPack(
    String packId, {
    required String idempotencyKey,
  }) => _fail('sticker.download');
}

class UnavailablePublisherPort implements PublisherPort {
  const UnavailablePublisherPort();

  @override
  Future<PageResult<Publisher>> publishers({
    String query = '',
    String cursor = '',
    int limit = 20,
  }) => _fail('publisher.list');

  @override
  Future<Publisher> setSubscribed(
    String publisherId,
    bool subscribed, {
    required int expectedVersion,
    required String idempotencyKey,
  }) => _fail('publisher.subscribe');

  @override
  Future<PageResult<Article>> articles(
    String publisherId, {
    String cursor = '',
    int limit = 20,
  }) => _fail('publisher.articles');

  @override
  Future<Article> article(String articleId, {required int docVersion}) =>
      _fail('publisher.article');
}

class UnavailableVideoPort implements VideoPort {
  const UnavailableVideoPort();

  @override
  Future<PageResult<VideoItem>> feed({String cursor = '', int limit = 20}) =>
      _fail('video.feed');

  @override
  Future<VideoItem> video(String videoId) => _fail('video.get');

  @override
  Future<String> playUrl(String videoId) => _fail('video.play');

  @override
  Future<void> stopPlayback() => _fail('video.stop');
}

class UnavailableLivePort implements LivePort {
  const UnavailableLivePort();

  @override
  Future<LiveSession> session(String channel) => _fail('live.state');

  @override
  Future<LiveSession> start(
    String channel,
    String title, {
    required String idempotencyKey,
  }) => _fail('live.start');

  @override
  Future<LiveSession> stop(String channel, {required String idempotencyKey}) =>
      _fail('live.stop');

  @override
  Future<LiveSession> join(String channel, {required String idempotencyKey}) =>
      _fail('live.join');

  @override
  Future<LiveSession> leave(String channel, {required String idempotencyKey}) =>
      _fail('live.leave');

  @override
  Future<void> danmaku(
    String channel,
    String text, {
    required String idempotencyKey,
  }) => _fail('live.danmaku');

  @override
  Stream<LiveSession> updates(String channel) =>
      Stream<LiveSession>.error(G13Failure.upstreamNotWired('live.updates'));
}

class UnavailableCallPort implements CallPort {
  const UnavailableCallPort();

  @override
  Future<CallSnapshot?> current() => _fail('call.current');

  @override
  Stream<CallSnapshot> updates() =>
      Stream<CallSnapshot>.error(G13Failure.upstreamNotWired('call.updates'));

  @override
  Future<CallSnapshot> start(
    String convId, {
    required bool video,
    required String idempotencyKey,
  }) => _fail('call.start');

  @override
  Future<CallSnapshot> answer(
    String callId, {
    required String idempotencyKey,
  }) => _fail('call.answer');

  @override
  Future<CallSnapshot> hangUp(
    String callId, {
    required String idempotencyKey,
  }) => _fail('call.hangup');

  @override
  Future<CallSnapshot> setMuted(
    String callId,
    bool muted, {
    required String idempotencyKey,
  }) => _fail('call.mute');

  @override
  Future<CallSnapshot> setSpeaker(
    String callId,
    bool speaker, {
    required String idempotencyKey,
  }) => _fail('call.speaker');

  @override
  Future<CallSnapshot> setCamera(
    String callId,
    bool on, {
    required String idempotencyKey,
  }) => _fail('call.camera');

  @override
  Future<CallSnapshot> setBluetooth(
    String callId,
    bool on, {
    required String idempotencyKey,
  }) => _fail('call.bluetooth');

  @override
  Future<CallSnapshot> flipCamera(
    String callId, {
    required String idempotencyKey,
  }) => _fail('call.flip');

  @override
  Future<CallSnapshot> setMinimized(String callId, bool minimized) =>
      _fail('call.minimize');

  @override
  Future<void> releaseMedia() => _fail('call.releaseMedia');
}

class UnavailableCommercePort implements CommercePort {
  const UnavailableCommercePort();

  @override
  Future<WalletAccount> wallet() => _fail('commerce.wallet');

  @override
  Future<PageResult<BillEntry>> bills({String cursor = '', int limit = 20}) =>
      _fail('commerce.bills');

  @override
  Future<List<BankCard>> bankCards() => _fail('commerce.cards');

  @override
  Future<BankCard> addBankCard({
    required String cardToken,
    required String idempotencyKey,
  }) => _fail('commerce.addCard');

  @override
  Future<void> removeBankCard(
    String cardId, {
    required String idempotencyKey,
  }) => _fail('commerce.removeCard');

  @override
  Future<PaymentIntent> prepareTransfer(
    TransferDraft draft, {
    required String idempotencyKey,
  }) => _fail('commerce.prepareTransfer');

  @override
  Future<PaymentIntent> executePayment(
    String intentId, {
    required String idempotencyKey,
  }) => _fail('commerce.executePayment');

  @override
  Future<PaymentIntent> paymentIntent(String intentId) =>
      _fail('commerce.intent');

  @override
  Future<RedPacket> redPacket(String packetId) => _fail('commerce.redPacket');

  @override
  Future<RedPacket> createRedPacket({
    required String convId,
    required Money total,
    required int count,
    required String greeting,
    required String idempotencyKey,
  }) => _fail('commerce.createRedPacket');

  @override
  Future<RedPacket> claimRedPacket(
    String packetId, {
    required String idempotencyKey,
  }) => _fail('commerce.claimRedPacket');

  @override
  Future<PageResult<CardEntry>> entitlements({
    String cursor = '',
    int limit = 20,
  }) => _fail('commerce.entitlements');

  @override
  Future<CardEntry> redeem(
    String entitlementId, {
    required String merchantToken,
    required String idempotencyKey,
  }) => _fail('commerce.redeem');

  @override
  Future<String> paymentCode({
    required String kind,
    required String idempotencyKey,
  }) => _fail('commerce.paymentCode');

  @override
  Future<void> saveQrToAlbum(
    String payload, {
    required String idempotencyKey,
  }) => _fail('commerce.saveQr');
}

class UnavailableDiscoveryPort implements DiscoveryPort {
  const UnavailableDiscoveryPort();

  @override
  Future<ScanResult> decodeScan(String imageRef) => _fail('scan.decode');

  @override
  Future<RecognitionResult> recognize(
    String imageRef, {
    required String targetLanguage,
  }) => _fail('vision.recognize');

  @override
  Future<RecognitionResult> translate(
    String text, {
    required String targetLanguage,
  }) => _fail('vision.translate');

  @override
  Future<PageResult<NearbyPlace>> nearby({
    required String cell,
    String cursor = '',
    int limit = 20,
  }) => _fail('geo.nearby');

  @override
  Future<PageResult<ServiceItem>> services({
    String category = '',
    String cursor = '',
    int limit = 20,
  }) => _fail('service.catalog');

  @override
  Future<ServiceQuote> quote(String serviceId) => _fail('service.quote');

  @override
  Future<OperationHandle> createOrder(
    String quoteId, {
    required String idempotencyKey,
  }) => _fail('service.createOrder');

  @override
  Future<OperationHandle> order(String operationId) => _fail('service.order');
}

class UnavailableMiniPort implements MiniPort {
  const UnavailableMiniPort();

  @override
  Future<PageResult<MiniAppInfo>> recent({
    String cursor = '',
    int limit = 20,
  }) => _fail('mini.recent');

  @override
  Future<PageResult<MiniAppInfo>> catalog({
    String cursor = '',
    int limit = 20,
  }) => _fail('mini.catalog');

  @override
  Future<MiniAppInfo> install(String appId, {required String idempotencyKey}) =>
      _fail('mini.install');

  @override
  Future<List<String>> grantedScopes(String appId) => _fail('mini.grant');

  @override
  Future<OperationHandle> launch(
    String appId, {
    required String idempotencyKey,
  }) => _fail('mini.launch');

  @override
  Future<OperationHandle> operation(String operationId) =>
      _fail('mini.operation');
}

class UnavailableSearchPort implements SearchPort {
  const UnavailableSearchPort();

  @override
  Future<PageResult<SearchHit>> search(
    String query, {
    String type = '',
    String cursor = '',
    int limit = 20,
  }) => _fail('search.query');

  @override
  Future<List<String>> history() => _fail('search.history');

  @override
  Future<void> clearHistory({required String idempotencyKey}) =>
      _fail('search.clearHistory');
}

class UnavailableProfilePort implements ProfilePort {
  const UnavailableProfilePort();

  @override
  Future<ProfileInfo> profile() => _fail('profile.get');

  @override
  Future<ProfileInfo> updateProfile({
    required String name,
    required String signature,
    required int expectedVersion,
    required String idempotencyKey,
  }) => _fail('profile.update');

  @override
  Future<SecurityInfo> security() => _fail('profile.security');

  @override
  Future<StorageUsage> storage() => _fail('profile.storage');

  @override
  Future<StorageUsage> clearCache({required String idempotencyKey}) =>
      _fail('profile.clearCache');

  @override
  Future<OperationHandle> exportBackup({required String idempotencyKey}) =>
      _fail('backup.export');

  @override
  Future<BackupFile> backupResult(String operationId) => _fail('backup.result');

  @override
  Future<void> restoreBackup(
    String fileId, {
    required String verificationDigest,
    required String idempotencyKey,
  }) => _fail('backup.restore');
}

class UnavailablePreferencesPort implements PreferencesPort {
  const UnavailablePreferencesPort();

  @override
  Future<Preferences> load() => _fail('preferences.load');

  @override
  Future<Preferences> update(
    Preferences next, {
    required String idempotencyKey,
  }) => _fail('preferences.update');
}

class UnavailableStatusPort implements StatusPort {
  const UnavailableStatusPort();

  @override
  Future<UserStatus?> myStatus() => _fail('content.status.mine');

  @override
  Future<PageResult<UserStatus>> friendStatuses({
    String cursor = '',
    int limit = 20,
  }) => _fail('content.status.friends');

  @override
  Future<UserStatus> publishStatus({
    required String text,
    required String background,
    required String visibility,
    required Duration ttl,
    required String idempotencyKey,
  }) => _fail('content.status.publish');

  @override
  Future<void> clearStatus({required String idempotencyKey}) =>
      _fail('content.status.clear');
}

class UnavailableLocationPort implements LocationPort {
  const UnavailableLocationPort();

  @override
  Future<LocationShare?> currentPlace(String convId) =>
      _fail('location.current');

  @override
  Future<PageResult<NearbyPlace>> places(
    String query, {
    String cursor = '',
    int limit = 20,
  }) => _fail('location.places');

  @override
  Future<LocationShare> sendLocation(
    String convId,
    String placeId, {
    required String idempotencyKey,
  }) => _fail('location.send');

  @override
  Future<LocationShare> startSharing(
    String convId, {
    required int minutes,
    required String idempotencyKey,
  }) => _fail('location.share.start');

  @override
  Future<void> stopSharing(String shareId, {required String idempotencyKey}) =>
      _fail('location.share.stop');

  @override
  Future<List<LocationShare>> activeShares(String convId) =>
      _fail('location.share.list');
}

class UnavailableQrPort implements QrPort {
  const UnavailableQrPort();

  @override
  Future<MyQrCode> myCode(QrStyle style, {required String idempotencyKey}) =>
      _fail('contact.qr.get');

  @override
  Future<MyQrCode> rotate({required String idempotencyKey}) =>
      _fail('contact.qr.rotate');

  @override
  Future<void> saveToAlbum(String payload, {required String idempotencyKey}) =>
      _fail('contact.qr.save');
}

/// 组合根尚未挂上绑定时使用的一整包端口。
G13Ports unavailablePorts() => const G13Ports(
  format: UnavailableFormatPort(),
  content: UnavailableContentPort(),
  publisher: UnavailablePublisherPort(),
  video: UnavailableVideoPort(),
  live: UnavailableLivePort(),
  calls: UnavailableCallPort(),
  commerce: UnavailableCommercePort(),
  discovery: UnavailableDiscoveryPort(),
  mini: UnavailableMiniPort(),
  search: UnavailableSearchPort(),
  profile: UnavailableProfilePort(),
  preferences: UnavailablePreferencesPort(),
  status: UnavailableStatusPort(),
  location: UnavailableLocationPort(),
  qr: UnavailableQrPort(),
);
