import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../../platform/platform_ports.dart';
import '../../../shell/theme.dart';
import '../../../shell/widgets.dart';
import '../../../src/service.dart';
import '../auth_controller.dart';
import '../auth_models.dart';
import '../auth_ports.dart';

const _strings = {
  'zh': {
    'welcome': '欢迎回来',
    'subtitle': '一个生活方式\n连接每一个日常',
    'phoneLogin': '手机号登录',
    'phone': '手机号',
    'code': '验证码',
    'sendCode': '获取验证码',
    'resend': '重新发送',
    'login': '登录',
    'more': '更多登录方式',
    'agreePrefix': '已阅读并同意',
    'privacy': '隐私保护指引',
    'security': '登录信息由设备安全存储保护',
    'language': '简体中文',
  },
  'en': {
    'welcome': 'Welcome back',
    'subtitle': 'A way of life\nconnecting every day',
    'phoneLogin': 'Sign in with phone',
    'phone': 'Phone number',
    'code': 'Verification code',
    'sendCode': 'Send code',
    'resend': 'Resend',
    'login': 'Sign in',
    'more': 'More sign-in options',
    'agreePrefix': 'I have read and agree to the ',
    'privacy': 'Privacy Policy',
    'security': 'Credentials are protected by device secure storage',
    'language': 'English',
  },
};

class LoginPage extends StatefulWidget {
  const LoginPage({
    super.key,
    required this.identity,
    required this.account,
    required this.secrets,
    required this.onSignedIn,
    this.onSwitchServer,
    this.onOpenQrApproval,
  });

  final IdentityPort identity;
  final AccountPort account;
  final SecretStore secrets;
  final ValueChanged<SessionInfo> onSignedIn;

  /// 换服务器返回新的账号端口；为 null 时输入框只读。
  final Future<AccountPort> Function(String base)? onSwitchServer;

  /// 登录态下收到桌面扫码挑战时的入口；未登录时由上层暂存。
  final ValueChanged<String>? onOpenQrApproval;

  @override
  State<LoginPage> createState() => _LoginPageState();
}

class _LoginPageState extends State<LoginPage> {
  late final AuthController _auth = AuthController(
    identity: widget.identity,
    secrets: AuthSecretStore(widget.secrets),
  );
  final _phone = TextEditingController();
  final _code = TextEditingController();

  @override
  void initState() {
    super.initState();
    _auth.addListener(_rebuild);
    _auth.loadDocuments();
    _auth.restore();
  }

  void _rebuild() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _auth.removeListener(_rebuild);
    _auth.dispose();
    _phone.dispose();
    _code.dispose();
    super.dispose();
  }

  String _t(String key) => _strings[_auth.locale]?[key] ?? key;

  Future<void> _requestCode({bool resend = false}) async {
    await _auth.requestCode(resend: resend);
    if (!mounted) return;
    final challenge = _auth.challenge;
    if (challenge != null && challenge.delivery == SmsDelivery.sms) {
      _code.clear();
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('验证码已发送至 ${challenge.maskedPhone}')),
      );
    }
  }

  Future<void> _verify() async {
    final session = await _auth.verify();
    if (!mounted || session == null) return;
    widget.onSignedIn(session);
  }

  void _openDocuments() {
    Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => ConsentListPage(documents: _auth.documents),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final dark = Theme.of(context).brightness == Brightness.dark;
    final textColor = dark ? WxColors.darkText : WxColors.text;
    final subtle = dark ? WxColors.darkTextSecondary : WxColors.textSecondary;
    return Scaffold(
      backgroundColor: dark ? WxColors.darkBg : Colors.white,
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.fromLTRB(28, 8, 28, 24),
          children: [
            _topBar(subtle),
            const SizedBox(height: 28),
            Center(
              child: Container(
                width: 68,
                height: 68,
                decoration: BoxDecoration(
                  gradient: const LinearGradient(
                    colors: [Color(0xff0BD566), WxColors.brandDark],
                    begin: Alignment.topLeft,
                    end: Alignment.bottomRight,
                  ),
                  borderRadius: BorderRadius.circular(18),
                ),
                child: const Icon(Icons.chat_bubble_rounded, color: Colors.white, size: 38),
              ),
            ),
            const SizedBox(height: 20),
            Text(
              _t('welcome'),
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 24, fontWeight: FontWeight.w600, color: textColor),
            ),
            const SizedBox(height: 8),
            Text(
              _t('subtitle'),
              textAlign: TextAlign.center,
              style: TextStyle(fontSize: 12, height: 1.6, color: subtle),
            ),
            const SizedBox(height: 32),
            Text(_t('phoneLogin'), style: TextStyle(fontSize: 11, color: subtle)),
            const SizedBox(height: 8),
            _phoneRow(textColor, subtle),
            const SizedBox(height: 16),
            _codeRow(textColor, subtle),
            const SizedBox(height: 24),
            _loginButton(),
            const SizedBox(height: 10),
            TextButton(
              onPressed: _openAccountLogin,
              child: Text(_t('more'), style: const TextStyle(color: WxColors.link, fontSize: 12)),
            ),
            const SizedBox(height: 8),
            _consentBlock(subtle),
            const SizedBox(height: 24),
            Row(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Icon(Icons.shield_outlined, size: 12, color: subtle),
                const SizedBox(width: 4),
                Flexible(
                  child: Text(_t('security'), style: TextStyle(fontSize: 9, color: subtle)),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _topBar(Color subtle) {
    // 登录页是根路由，没有"关闭"可言；这里只留语言切换。
    return Align(
      alignment: Alignment.centerRight,
      child: PopupMenuButton<String>(
        key: const Key('login-language'),
        tooltip: '语言 / Language',
        onSelected: _auth.setLocale,
        itemBuilder: (_) => const [
          PopupMenuItem(value: 'zh', child: Text('简体中文')),
          PopupMenuItem(value: 'en', child: Text('English')),
        ],
        child: Text(
          _t('language'),
          style: const TextStyle(color: WxColors.link, fontSize: 11),
        ),
      ),
    );
  }

  Widget _phoneRow(Color textColor, Color subtle) {
    return Container(
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: subtle.withValues(alpha: 0.25))),
      ),
      padding: const EdgeInsets.only(bottom: 8),
      child: Row(
        children: [
          PopupMenuButton<CountryCode>(
            key: const Key('login-country'),
            enabled: !_auth.sending,
            onSelected: _auth.setCountry,
            itemBuilder: (_) => [
              for (final country in defaultCountries)
                PopupMenuItem(value: country, child: Text('${country.label}  ${country.name}')),
            ],
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text('+${_auth.country.dialCode}', style: TextStyle(fontSize: 15, color: textColor)),
                Icon(Icons.arrow_drop_down, size: 16, color: subtle),
              ],
            ),
          ),
          const SizedBox(width: 10),
          Container(width: 1, height: 16, color: subtle.withValues(alpha: 0.25)),
          const SizedBox(width: 10),
          Expanded(
            child: TextField(
              key: const Key('login-phone'),
              controller: _phone,
              enabled: !_auth.sending && !_auth.verifying,
              keyboardType: TextInputType.phone,
              inputFormatters: [FilteringTextInputFormatter.digitsOnly],
              onChanged: _auth.setPhone,
              style: TextStyle(fontSize: 16, color: textColor, letterSpacing: 0.4),
              decoration: InputDecoration(
                hintText: _t('phone'),
                hintStyle: TextStyle(color: subtle, fontSize: 15),
                filled: false,
                border: InputBorder.none,
                enabledBorder: InputBorder.none,
                focusedBorder: InputBorder.none,
                isDense: true,
                contentPadding: EdgeInsets.zero,
              ),
            ),
          ),
          if (_auth.phone.isNotEmpty)
            IconButton(
              onPressed: () {
                _phone.clear();
                _auth.setPhone('');
              },
              icon: Icon(Icons.cancel, size: 15, color: subtle),
            ),
        ],
      ),
    );
  }

  Widget _codeRow(Color textColor, Color subtle) {
    final challenge = _auth.challenge;
    final counting = _auth.remaining > Duration.zero;
    final label = challenge == null
        ? _t('sendCode')
        : counting
            ? '${_t('resend')} ${formatCountdown(_auth.remaining)}'
            : _t('resend');
    return Row(
      children: [
        Expanded(
          child: TextField(
            key: const Key('login-code'),
            controller: _code,
            enabled: challenge != null && !_auth.verifying,
            keyboardType: TextInputType.number,
            maxLength: 8,
            onChanged: _auth.setCode,
            style: TextStyle(fontSize: 15, color: textColor, letterSpacing: 2),
            decoration: InputDecoration(
              hintText: _t('code'),
              hintStyle: TextStyle(color: subtle, fontSize: 14),
              counterText: '',
              filled: false,
              border: InputBorder.none,
              enabledBorder: InputBorder.none,
              focusedBorder: InputBorder.none,
              isDense: true,
              contentPadding: EdgeInsets.zero,
            ),
          ),
        ),
        const SizedBox(width: 8),
        TextButton(
          key: const Key('login-send-code'),
          onPressed: counting || _auth.sending || !_auth.phoneValid
              ? null
              : () => _requestCode(resend: challenge != null),
          style: TextButton.styleFrom(
            backgroundColor: counting ? subtle.withValues(alpha: 0.12) : WxColors.brandSoft,
            foregroundColor: counting ? subtle : WxColors.brandDark,
            shape: const StadiumBorder(),
            padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
          ),
          child: _auth.sending
              ? const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2))
              : Text(label, style: const TextStyle(fontSize: 11)),
        ),
      ],
    );
  }

  Widget _loginButton() {
    final enabled = _auth.canVerify;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        if (_auth.failure != null) ...[
          G12ErrorBanner(
            message: _auth.errorText,
            onRetry: _auth.failure!.retryable ? () => _requestCode() : null,
          ),
          const SizedBox(height: 12),
        ],
        FilledButton(
          key: const Key('login-submit'),
          onPressed: enabled ? _verify : null,
          style: FilledButton.styleFrom(
            disabledBackgroundColor: const Color(0xffF0F0F0),
            disabledForegroundColor: WxColors.textTertiary,
            minimumSize: const Size.fromHeight(48),
          ),
          child: _auth.verifying
              ? const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
              : Text(_t('login'), style: const TextStyle(fontSize: 15, fontWeight: FontWeight.w500)),
        ),
      ],
    );
  }

  Widget _consentBlock(Color subtle) {
    final docs = _auth.documents;
    if (_auth.loadingDocuments) {
      return const Center(child: SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)));
    }
    if (docs.isEmpty) {
      return Row(
        children: [
          Expanded(
            child: Text(
              '协议文档未加载（identity.consent_documents）。未同意协议不能登录。',
              style: TextStyle(fontSize: 10, height: 1.5, color: subtle),
            ),
          ),
          TextButton(onPressed: _auth.loadDocuments, child: const Text('加载')),
        ],
      );
    }
    return Column(
      children: [
        for (final doc in docs)
          InkWell(
            key: Key('consent-${doc.id}'),
            onTap: () => _auth.toggleConsent(doc.id, !_auth.isAgreed(doc.id)),
            child: Padding(
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Checkbox(
                    value: _auth.isAgreed(doc.id),
                    onChanged: (value) => _auth.toggleConsent(doc.id, value ?? false),
                    visualDensity: VisualDensity.compact,
                  ),
                  Expanded(
                    child: Padding(
                      padding: const EdgeInsets.only(top: 11),
                      child: Text(
                        '${doc.title}（v${doc.version}）',
                        style: TextStyle(
                          fontSize: 10,
                          height: 1.5,
                          color: WxColors.link,
                          decoration: TextDecoration.underline,
                          decorationColor: WxColors.link,
                        ),
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        Align(
          alignment: Alignment.centerLeft,
          child: TextButton(
            key: const Key('login-open-consents'),
            onPressed: _openDocuments,
            child: const Text('查看协议全文', style: TextStyle(fontSize: 11)),
          ),
        ),
      ],
    );
  }

  /// 账号密码通道在发布版也必须可达：identity（短信/扫码）未接线时，
  /// 这是唯一能登录的路径，不能只在 debug 里放行。
  Future<void> _openAccountLogin() async {
    await Navigator.of(context).push(
      MaterialPageRoute<void>(
        builder: (_) => AccountLoginPage(
          account: widget.account,
          onSwitchServer: widget.onSwitchServer,
          onSignedIn: widget.onSignedIn,
        ),
      ),
    );
  }
}

class AccountLoginPage extends StatefulWidget {
  const AccountLoginPage({
    super.key,
    required this.account,
    required this.onSignedIn,
    this.onSwitchServer,
  });

  final AccountPort account;
  final ValueChanged<SessionInfo> onSignedIn;
  final Future<AccountPort> Function(String base)? onSwitchServer;

  @override
  State<AccountLoginPage> createState() => _AccountLoginPageState();
}

class _AccountLoginPageState extends State<AccountLoginPage> {
  final _server = TextEditingController(text: 'http://127.0.0.1:8099');
  final _name = TextEditingController();
  final _password = TextEditingController();
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    _server.dispose();
    _name.dispose();
    _password.dispose();
    super.dispose();
  }

  Future<void> _run(bool login) async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      var account = widget.account;
      final base = _server.text.trim();
      if (base.isNotEmpty && widget.onSwitchServer != null) {
        account = await widget.onSwitchServer!(base);
      }
      final session = login
          ? await account.login(_name.text.trim(), _password.text)
          : await account.register(_name.text.trim(), _password.text);
      if (!mounted) return;
      widget.onSignedIn(session);
      Navigator.of(context).popUntil((route) => route.isFirst);
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final ready = _name.text.trim().isNotEmpty && _password.text.isNotEmpty && !_busy;
    return Scaffold(
      appBar: AppBar(title: const Text('账号密码登录')),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          TextField(
            controller: _server,
            enabled: widget.onSwitchServer != null && !_busy,
            keyboardType: TextInputType.url,
            decoration: const InputDecoration(
              labelText: '服务器地址',
              helperText: '默认服务器由 WXLIKE_SERVER 覆盖；真机联调填开发机局域网地址',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _name,
            onChanged: (_) => setState(() {}),
            decoration: const InputDecoration(labelText: '用户名', border: OutlineInputBorder()),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _password,
            onChanged: (_) => setState(() {}),
            obscureText: true,
            decoration: const InputDecoration(labelText: '密码', border: OutlineInputBorder()),
          ),
          if (_error != null) ...[
            const SizedBox(height: 16),
            G12ErrorBanner(message: _error!),
          ],
          const SizedBox(height: 20),
          FilledButton(
            onPressed: ready ? () => _run(true) : null,
            child: _busy
                ? const SizedBox(height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Text('登录'),
          ),
          const SizedBox(height: 8),
          OutlinedButton(onPressed: ready ? () => _run(false) : null, child: const Text('注册新账号')),
        ],
      ),
    );
  }
}

class ConsentListPage extends StatelessWidget {
  const ConsentListPage({super.key, required this.documents});

  final List<ConsentDocument> documents;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('服务协议与隐私')),
      body: ListView.separated(
        itemCount: documents.length,
        separatorBuilder: (_, _) => const Divider(height: 1),
        itemBuilder: (context, index) {
          final doc = documents[index];
          return ListTile(
            title: Text(doc.title),
            subtitle: Text('版本 ${doc.version}${doc.requiredToLogin ? ' · 登录必读' : ''}'),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(
              MaterialPageRoute<void>(builder: (_) => ConsentDetailPage(document: doc)),
            ),
          );
        },
      ),
    );
  }
}

class ConsentDetailPage extends StatelessWidget {
  const ConsentDetailPage({super.key, required this.document});

  final ConsentDocument document;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(document.title)),
      body: ListView(
        padding: const EdgeInsets.all(16),
        children: [
          Text('版本 ${document.version}', style: Theme.of(context).textTheme.labelMedium),
          const SizedBox(height: 12),
          if (document.body.isNotEmpty)
            SelectableText(document.body)
          else
            Text(
              document.url.isEmpty
                  ? '正文未下发。协议内容由服务端 identity.consent_documents 提供，不能由客户端补写。'
                  : document.url,
              style: const TextStyle(color: WxColors.link),
            ),
        ],
      ),
    );
  }
}
