/// G12 的组合根：把 Rust 端口接到产品壳上，决定登录页与会话页的切换。
///
/// 它不判断业务规则：会话能不能发、未读是多少、离线显示什么，全由 Rust
/// 返回的记录决定。这里只做生命周期、导航与"上游未接线时明确显示原因"。
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../features/auth/auth_ports.dart';
import '../features/auth/challenge/login_page.dart';
import '../features/auth/qr_approval/qr_approval_page.dart';
import '../features/chat/composer/composer_ports.dart';
import '../features/chat/composer/draft_store.dart';
import '../features/chat/timeline/conversations_controller.dart';
import '../features/chat/timeline/conversations_page.dart';
import '../features/chat/timeline/messaging_ports.dart';
import '../features/chat/timeline/timeline_controller.dart';
import '../features/chat/timeline/timeline_page.dart';
import '../features/contacts/contacts_controller.dart';
import '../features/contacts/contacts_page.dart';
import '../features/contacts/group_page.dart';
import '../features/contacts/profile_page.dart';
import '../features/contacts/relations_ports.dart';
import '../platform/platform_ports.dart';
import '../src/service.dart';
import 'account_page.dart';
import 'app_lock.dart';
import 'contracts.dart';
import 'app_shell.dart';
import 'theme.dart';
import 'widgets.dart';

class WxlikeMobileApp extends StatefulWidget {
  const WxlikeMobileApp({
    super.key,
    required this.account,
    required this.session,
    required this.messaging,
    required this.platform,
    required this.push,
    required this.lifecycle,
    required this.secrets,
    this.identity = const UnavailableIdentityPort(),
    this.attachments = const UnavailableAttachmentPort(),
    this.redPackets = const UnavailableRedPacketComposerPort(),
    this.entries = const UnavailableComposerEntryPort(),
    this.actions = const UnavailableMessageActionsPort(),
    this.relations = const UnavailableRelationsPort(),
    this.deepLinks,
    this.discoverBuilder,
    this.contactsBuilder,
    this.onSwitchServer,
  });

  final AccountPort account;
  final SessionInfo session;
  final MessagingPort messaging;
  final PlatformPort platform;
  final PushPort push;
  final LifecyclePort lifecycle;
  final SecretStore secrets;
  final IdentityPort identity;
  final AttachmentPort attachments;
  final RedPacketComposerPort redPackets;
  final ComposerEntryPort entries;
  final MessageActionsPort actions;
  final RelationsPort relations;
  final Stream<Uri>? deepLinks;
  final WidgetBuilder? discoverBuilder;

  /// G12-04 接入前由调用方注入；为空时显示明确的阻塞原因。
  final WidgetBuilder? contactsBuilder;
  final Future<AccountPort> Function(String base)? onSwitchServer;

  @override
  State<WxlikeMobileApp> createState() => _WxlikeMobileAppState();
}

class _WxlikeMobileAppState extends State<WxlikeMobileApp> {
  final _shellKey = GlobalKey<AppShellState>();
  final _messengerKey = GlobalKey<ScaffoldMessengerState>();
  final _drafts = DraftStore();

  late AppLockController _lock;
  StreamSubscription<Uri>? _linkSub;
  SessionInfo? _session;
  ConversationsController? _conversations;
  late final ContactsController _contacts = ContactsController(port: widget.relations);
  Uri? _pendingLink;
  bool _pushSynced = false;

  @override
  void initState() {
    super.initState();
    _drafts.load();
    _linkSub = widget.deepLinks?.listen((uri) {
      final shell = _shellKey.currentState;
      if (shell != null) {
        shell.handleDeepLink(uri);
      } else {
        _onDeepLinkBeforeLogin(uri);
      }
    });
    _session = widget.session.signedIn ? widget.session : null;
    _lock = AppLockController(secrets: widget.secrets)..restore();
    if (_session != null) {
      _mountConversations();
      _syncPush();
    }
  }

  @override
  void dispose() {
    _linkSub?.cancel();
    _conversations?.dispose();
    _contacts.dispose();
    _drafts.dispose();
    _lock.dispose();
    super.dispose();
  }

  void _mountConversations() {
    _conversations?.dispose();
    _conversations = ConversationsController(port: widget.messaging);
  }

  Future<void> _onSignedIn(SessionInfo session) async {
    setState(() {
      _session = session;
      _mountConversations();
    });
    await _syncPush();
    final pending = _pendingLink;
    _pendingLink = null;
    if (pending != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _shellKey.currentState?.handleDeepLink(pending));
    }
  }

  Future<void> _onSignOut() async {
    try {
      await widget.push.revoke();
    } catch (_) {
      // 撤销失败不能让退出登录卡住；Rust 侧仍会清掉本机会话。
    }
    await widget.account.logout();
    if (!mounted) return;
    final oldConversations = _conversations;
    setState(() {
      _session = null;
      _conversations = null;
      _pushSynced = false;
    });
    // 页面已不再持有控制器；等壳从树上移除后再释放订阅。
    WidgetsBinding.instance.addPostFrameCallback((_) => oldConversations?.dispose());
  }

  Future<void> _syncPush() async {
    if (_pushSynced) return;
    try {
      final token = await widget.push.token();
      if (token == null) return;
      await widget.account.registerPushToken(token.platform, token.token);
      _pushSynced = true;
    } catch (error) {
      if (!mounted) return;
      _notifyUser('推送登记失败：${asG12Failure(error, operation: '登记推送').displayText}');
    }
  }

  void _onDeepLinkBeforeLogin(Uri uri) {
    if (!uri.toString().startsWith('wxlike://')) return;
    _pendingLink = uri;
    _notifyUser('请先登录后再打开链接');
  }

  /// 弹全局 snackbar。MaterialApp 首帧之前（冷启动带着会话直奔 [_syncPush]）
  /// messenger 还不存在：等一帧再弹，别把提示静默吞掉。
  void _notifyUser(String message) {
    final messenger = _messengerKey.currentState;
    if (messenger != null) {
      messenger.showSnackBar(SnackBar(content: Text(message)));
      return;
    }
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      _messengerKey.currentState?.showSnackBar(SnackBar(content: Text(message)));
    });
  }

  @override
  Widget build(BuildContext context) {
    final session = _session;
    return MaterialApp(
      title: 'wxlike',
      debugShowCheckedModeBanner: false,
      scaffoldMessengerKey: _messengerKey,
      theme: WxTheme.light(),
      darkTheme: WxTheme.dark(),
      themeMode: ThemeMode.system,
      home: session == null ? _login() : _shell(session),
    );
  }

  Widget _login() {
    return LoginPage(
      identity: widget.identity,
      account: widget.account,
      secrets: widget.secrets,
      onSignedIn: _onSignedIn,
      onSwitchServer: widget.onSwitchServer,
      onOpenQrApproval: (challengeId) => _onDeepLinkBeforeLogin(
        Uri.parse('wxlike://qr/$challengeId'),
      ),
    );
  }

  Widget _shell(SessionInfo session) {
    final conversations = _conversations!;
    return AppShell(
      key: _shellKey,
      lock: _lock,
      lifecycle: widget.lifecycle,
      onSignOut: _onSignOut,
      deepLinks: null,
      onResumed: () => conversations.setForeground(true),
      onPaused: () {
        conversations.setForeground(false);
        _drafts.flush();
      },
      chatPageBuilder: (context, conv) => _timeline(conv),
      qrApprovalPageBuilder: (context, challengeId) => QrApprovalPage(
        identity: widget.identity,
        challengeId: challengeId,
        session: session,
      ),
      tabs: [
        ShellTabSpec(
          label: '微信',
          icon: Icons.chat_bubble_outline,
          selectedIcon: Icons.chat_bubble,
          builder: (context) => ConversationsPage(
            controller: conversations,
            drafts: _drafts,
            onOpenChat: (row) => Navigator.of(context).push(
              MaterialPageRoute<void>(builder: (_) => _timeline(row.conv, title: row.title, isGroup: row.kind == 'group')),
            ),
            onOpenScan: () => _messengerKey.currentState?.showSnackBar(
              const SnackBar(content: Text('扫码入口由 G11 提供；手机端只处理扫码登录挑战')),
            ),
          ),
        ),
        ShellTabSpec(
          label: '通讯录',
          icon: Icons.contacts_outlined,
          selectedIcon: Icons.contacts,
          builder: widget.contactsBuilder ??
          (context) => ContactsPage(
                controller: _contacts,
                onOpenProfile: (userId) => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => ContactProfilePage(port: widget.relations, userId: userId),
                  ),
                ),
                onOpenGroup: (groupId) => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => GroupPage(port: widget.relations, groupId: groupId),
                  ),
                ),
                onOpenChat: (contact) => Navigator.of(context).push(
                  MaterialPageRoute<void>(
                    builder: (_) => _timeline(contact.userId, title: contact.title),
                  ),
                ),
              ),
        ),
        ShellTabSpec(
          label: '发现',
          icon: Icons.explore_outlined,
          selectedIcon: Icons.explore,
          builder: widget.discoverBuilder ?? _blockedDiscover,
        ),
        ShellTabSpec(
          label: '我',
          icon: Icons.person_outline,
          selectedIcon: Icons.person,
          builder: (context) => AccountPage(
            account: widget.account,
            session: session,
            lock: _lock,
            push: widget.push,
            onSignOut: _onSignOut,
          ),
        ),
      ],
    );
  }

  Widget _timeline(String conv, {String title = '会话', bool isGroup = false}) {
    // TimelineController 必须每个路由只建一次：路由 builder 每次重建都会
    // 重新执行，直接在 builder 里 new 一个控制器会丢消息状态、漏掉订阅，
    // 还把旧控制器变成没人 dispose 的死对象。
    return _TimelineScreen(
      port: widget.messaging,
      conv: conv,
      pageBuilder: (controller) => TimelinePage(
        controller: controller,
        drafts: _drafts,
        platform: widget.platform,
        attachments: widget.attachments,
        redPackets: widget.redPackets,
        entries: widget.entries,
        actions: widget.actions,
        lifecycle: widget.lifecycle,
        networkStatus: widget.platform.networkStatus(),
        title: title,
        isGroup: isGroup,
      ),
    );
  }

  Widget _blockedDiscover(BuildContext context) {
    return const G12BlockedState(
      title: '发现',
      reason: '朋友圈、视频与小程序页由 G13 提供，本壳只负责路由槽位。',
    );
  }
}

/// 让 [TimelineController] 的生命周期对齐路由（而不是对齐某一次 build）：
/// 控制器在 initState 里创建一次，由 [TimelinePage] 负责释放。
class _TimelineScreen extends StatefulWidget {
  const _TimelineScreen({
    required this.port,
    required this.conv,
    required this.pageBuilder,
  });

  final MessagingPort port;
  final String conv;
  final TimelinePage Function(TimelineController controller) pageBuilder;

  @override
  State<_TimelineScreen> createState() => _TimelineScreenState();
}

class _TimelineScreenState extends State<_TimelineScreen> {
  late final TimelineController _controller = TimelineController(
    port: widget.port,
    conv: widget.conv,
  );

  @override
  Widget build(BuildContext context) => widget.pageBuilder(_controller);
}
