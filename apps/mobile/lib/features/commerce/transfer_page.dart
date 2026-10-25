/// G13-03：转账。
///
/// 12 键键盘（0-9、小数点、删除）、金额预设、备注上限、确认弹窗、更换付款
/// 方式。金额在本地只做十进制文本 → 最小单位整数的转换，显示一律走 Rust
/// 格式化。确认后先 prepare 再 execute，只有服务端返回 SUCCEEDED 才回聊天，
/// 并在原会话插入业务卡片。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class TransferPage extends StatefulWidget {
  const TransferPage({
    super.key,
    required this.ports,
    required this.convId,
    required this.payeeId,
    required this.payeeName,
  });

  final G13Ports ports;
  final String convId;
  final String payeeId;
  final String payeeName;

  @override
  State<TransferPage> createState() => _TransferPageState();
}

class _TransferPageState extends State<TransferPage> with SubmitGuard {
  static const int noteLimit = 20;
  static const List<int> presets = [100, 200, 500];

  String _amount = '';
  final TextEditingController _note = TextEditingController();
  List<BankCard>? _cards;
  BankCard? _selectedCard;
  String? _error;

  /// 一次"确认转账"意图对应一次 prepare；重试 execute 复用同一个键。
  String _executeKey = newIdempotencyKey();

  @override
  void initState() {
    super.initState();
    _loadCards();
  }

  Future<void> _loadCards() async {
    try {
      final cards = await widget.ports.commerce.bankCards();
      if (!mounted) return;
      setState(() {
        _cards = cards;
        _selectedCard = _defaultCard(cards);
      });
    } catch (_) {
      // 绑卡列表读不到不影响余额付款；付款方式入口会显示"默认余额"。
      if (mounted) setState(() => _cards = const []);
    }
  }

  @override
  void dispose() {
    _note.dispose();
    super.dispose();
  }

  static BankCard? _defaultCard(List<BankCard> cards) {
    for (final card in cards) {
      if (card.isDefault) return card;
    }
    return null;
  }

  String get _paymentMethodId => _selectedCard?.id ?? 'balance';

  void _resetIntent() {
    // 金额/备注/付款方式变了就是新的用户意图：旧幂等键带着旧参数会被
    // 服务端判 CONFLICT，不能复用。
    _executeKey = newIdempotencyKey();
  }

  Money? get _money {
    final minor = parseMinorAmount(_amount);
    if (minor == null) return null;
    return Money(minor: minor, currency: 'CNY');
  }

  bool get _canSubmit => !submitting && (_money?.minor ?? 0) > 0;

  void _press(String key) {
    setState(() {
      _error = null;
      _resetIntent();
      if (key == 'del') {
        if (_amount.isNotEmpty) {
          _amount = _amount.substring(0, _amount.length - 1);
        }
        return;
      }
      if (key == '.') {
        if (_amount.contains('.')) return;
        _amount = _amount.isEmpty ? '0.' : '$_amount.';
        return;
      }
      if (_amount.contains('.')) {
        final fraction = _amount.split('.').last;
        if (fraction.length >= 2) return;
      }
      if (_amount == '0') {
        _amount = key;
        return;
      }
      if (_amount.length >= 12) return;
      _amount += key;
    });
  }

  Future<void> _submit() async {
    final money = _money;
    if (money == null || money.minor <= 0) return;
    await guard(() async {
      try {
        final intent = await widget.ports.commerce.prepareTransfer(
          TransferDraft(
            convId: widget.convId,
            payeeId: widget.payeeId,
            payeeName: widget.payeeName,
            amount: money,
            note: _note.text.trim(),
            paymentMethodId: _paymentMethodId,
          ),
          idempotencyKey:
              'transfer-prepare-${widget.convId}-${widget.payeeId}-${money.minor}-'
              '${_note.text.trim().hashCode}-$_paymentMethodId',
        );
        if (!mounted) return;
        final confirmed = await _confirm(intent);
        if (!confirmed) return;
        final result = await widget.ports.commerce.executePayment(
          intent.id,
          idempotencyKey: _executeKey,
        );
        if (!mounted) return;
        if (result.succeeded) {
          Navigator.of(context).pop(result);
          return;
        }
        setState(() => _error = '支付状态：${result.state}');
      } catch (error) {
        if (!mounted) return;
        setState(
          () => _error = error is G13Failure ? error.displayText : '$error',
        );
      }
    });
  }

  Future<bool> _confirm(PaymentIntent intent) async {
    final confirmation = intent.confirmation;
    final result = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('确认转账'),
        content: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              mainAxisAlignment: MainAxisAlignment.spaceBetween,
              children: [
                const Text('金额'),
                MoneyText(
                  money: intent.amount,
                  format: widget.ports.format,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ],
            ),
            const SizedBox(height: 8),
            Text('收款方：${intent.payee}'),
            if (confirmation != null && confirmation.summary.isNotEmpty) ...[
              const SizedBox(height: 8),
              Text(
                confirmation.summary,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
            const SizedBox(height: 8),
            Text(
              '付款方式：${_methodLabel()}',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ],
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('确认转账'),
          ),
        ],
      ),
    );
    return result ?? false;
  }

  String _methodLabel() => _selectedCard == null
      ? '余额'
      : '${_selectedCard!.bank} ${_selectedCard!.maskedNumber}';

  Future<void> _changeMethod() async {
    final cards = _cards ?? const <BankCard>[];
    final selected = await showModalBottomSheet<String>(
      context: context,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            ListTile(
              leading: const Icon(Icons.account_balance_wallet),
              title: const Text('余额'),
              trailing: _selectedCard == null ? const Icon(Icons.check) : null,
              onTap: () => Navigator.of(context).pop('balance'),
            ),
            for (final card in cards)
              ListTile(
                leading: const Icon(Icons.credit_card),
                title: Text('${card.bank} ${card.maskedNumber}'),
                trailing: _selectedCard?.id == card.id
                    ? const Icon(Icons.check)
                    : null,
                onTap: () => Navigator.of(context).pop(card.id),
              ),
            if (cards.isEmpty)
              const Padding(
                padding: EdgeInsets.all(16),
                child: Text('还没有绑定银行卡，当前使用余额付款。'),
              ),
          ],
        ),
      ),
    );
    if (selected == null) return;
    setState(() {
      _resetIntent();
      _selectedCard = selected == 'balance' ? null : _cardById(cards, selected);
    });
  }

  static BankCard? _cardById(List<BankCard> cards, String id) {
    for (final card in cards) {
      if (card.id == id) return card;
    }
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final money = _money;
    return G13Scaffold(
      title: '转账',
      subtitle: '给 ${widget.payeeName}',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        TextButton(onPressed: _changeMethod, child: const Text('付款方式')),
      ],
      body: Column(
        children: [
          Expanded(
            child: SingleChildScrollView(
              padding: const EdgeInsets.all(20),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('金额', style: Theme.of(context).textTheme.bodySmall),
                  const SizedBox(height: 6),
                  Row(
                    crossAxisAlignment: CrossAxisAlignment.end,
                    children: [
                      Text(
                        '¥',
                        style: Theme.of(context).textTheme.headlineSmall,
                      ),
                      const SizedBox(width: 6),
                      Text(
                        money == null ? '0.00' : _display(money),
                        style: Theme.of(context).textTheme.displaySmall,
                      ),
                    ],
                  ),
                  const SizedBox(height: 12),
                  Wrap(
                    spacing: 8,
                    children: [
                      for (final preset in presets)
                        ActionChip(
                          label: Text('¥$preset'),
                          onPressed: () => setState(() {
                            _resetIntent();
                            _amount = '$preset';
                          }),
                        ),
                    ],
                  ),
                  const SizedBox(height: 20),
                  TextField(
                    controller: _note,
                    maxLength: noteLimit,
                    decoration: const InputDecoration(
                      labelText: '转账说明',
                      hintText: '选填',
                      border: OutlineInputBorder(),
                    ),
                    onChanged: (_) {
                      setState(_resetIntent);
                    },
                  ),
                  ListTile(
                    contentPadding: EdgeInsets.zero,
                    leading: const Icon(Icons.credit_card),
                    title: const Text('付款方式'),
                    subtitle: Text(_methodLabel()),
                    trailing: const Icon(Icons.chevron_right),
                    onTap: _changeMethod,
                  ),
                  if (_error != null) ...[
                    const SizedBox(height: 8),
                    InlineFailure(
                      error: G13Failure(
                        G13FailureKind.unexpected,
                        'TRANSFER_STATE',
                        _error!,
                      ),
                    ),
                  ],
                ],
              ),
            ),
          ),
          _Keypad(onKey: _press),
          SafeArea(
            top: false,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 12),
              child: SizedBox(
                width: double.infinity,
                child: FilledButton(
                  onPressed: _canSubmit ? _submit : null,
                  child: submitting
                      ? const SizedBox(
                          width: 18,
                          height: 18,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Text('转账'),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }

  String _display(Money money) {
    try {
      return widget.ports.format.money(money);
    } catch (_) {
      return '—';
    }
  }
}

class _Keypad extends StatelessWidget {
  const _Keypad({required this.onKey});

  final void Function(String key) onKey;

  static const _keys = <String>[
    '1',
    '2',
    '3',
    '4',
    '5',
    '6',
    '7',
    '8',
    '9',
    '.',
    '0',
    'del',
  ];

  @override
  Widget build(BuildContext context) {
    return Container(
      color: Theme.of(context).colorScheme.surfaceContainerHighest,
      padding: const EdgeInsets.all(4),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: GridView.count(
            crossAxisCount: 3,
            shrinkWrap: true,
            physics: const NeverScrollableScrollPhysics(),
            childAspectRatio: 2.2,
            mainAxisSpacing: 4,
            crossAxisSpacing: 4,
            children: [
              for (final key in _keys)
                Material(
                  color: Theme.of(context).colorScheme.surface,
                  borderRadius: BorderRadius.circular(6),
                  child: InkWell(
                    borderRadius: BorderRadius.circular(6),
                    onTap: () => onKey(key),
                    child: Center(
                      child: key == 'del'
                          ? const Icon(Icons.backspace_outlined)
                          : Text(key, style: const TextStyle(fontSize: 20)),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
