/// G13-03：钱包、账单、银行卡。
///
/// 余额、冻结状态、账单翻页都来自 `commerce.*`；虚拟币与真钱分开显示，
/// 不合成一个"总资产"。刷新不会退回示例金额——没有数据就显示失败或空，
/// 不显示 66.66 这类演示值。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';
import 'cards_page.dart';
import 'pay_page.dart';

class WalletPage extends StatefulWidget {
  const WalletPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<WalletPage> createState() => _WalletPageState();
}

class _WalletPageState extends State<WalletPage> {
  late Future<WalletAccount> _future = widget.ports.commerce.wallet();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.commerce.wallet();
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '钱包',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<WalletAccount>(
        future: _future,
        onRetry: _reload,
        builder: (context, account) => ListView(
          children: [
            if (account.frozen)
              const Padding(
                padding: EdgeInsets.all(12),
                child: InlineFailure(
                  error: G13Failure(
                    G13FailureKind.forbidden,
                    'WALLET_FROZEN',
                    '账户已冻结，资金操作暂不可用',
                  ),
                ),
              ),
            Padding(
              padding: const EdgeInsets.fromLTRB(20, 20, 20, 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text('余额', style: Theme.of(context).textTheme.bodySmall),
                  const SizedBox(height: 4),
                  MoneyText(
                    money: account.balance,
                    format: widget.ports.format,
                    style: Theme.of(context).textTheme.headlineMedium,
                  ),
                  const SizedBox(height: 8),
                  Text(
                    '虚拟币 ${account.virtualBalance.currency}',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  MoneyText(
                    money: account.virtualBalance,
                    format: widget.ports.format,
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ],
              ),
            ),
            const Divider(),
            _ActionTile(
              icon: Icons.qr_code_2,
              title: '收付款',
              subtitle: '付款码与收款',
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => PayPage(ports: widget.ports),
                ),
              ),
            ),
            _ActionTile(
              icon: Icons.receipt_long,
              title: '账单',
              subtitle: '全部收支记录',
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => BillsPage(ports: widget.ports),
                ),
              ),
            ),
            _ActionTile(
              icon: Icons.credit_card,
              title: '银行卡',
              subtitle: '绑卡、解绑与默认卡',
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => BankCardsPage(ports: widget.ports),
                ),
              ),
            ),
            _ActionTile(
              icon: Icons.card_giftcard,
              title: '卡包',
              subtitle: '会员卡、券与票证',
              onTap: () => Navigator.of(context).push(
                MaterialPageRoute<void>(
                  builder: (_) => CardsPage(ports: widget.ports),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _ActionTile extends StatelessWidget {
  const _ActionTile({
    required this.icon,
    required this.title,
    required this.subtitle,
    required this.onTap,
  });

  final IconData icon;
  final String title;
  final String subtitle;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: Icon(icon),
      title: Text(title),
      subtitle: Text(subtitle),
      trailing: const Icon(Icons.chevron_right),
      onTap: onTap,
    );
  }
}

class BillsPage extends StatefulWidget {
  const BillsPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<BillsPage> createState() => _BillsPageState();
}

class _BillsPageState extends State<BillsPage> {
  final List<BillEntry> _items = <BillEntry>[];
  String _cursor = '';
  bool _hasMore = true;
  bool _loading = true;
  Object? _error;

  @override
  void initState() {
    super.initState();
    _load(reset: true);
  }

  Future<void> _load({required bool reset}) async {
    if (!reset && (!_hasMore || _loading)) return;
    setState(() {
      _loading = true;
      _error = null;
    });
    try {
      final page = await widget.ports.commerce.bills(
        cursor: reset ? '' : _cursor,
      );
      if (!mounted) return;
      setState(() {
        if (reset) _items.clear();
        _items.addAll(page.items);
        _cursor = page.nextCursor;
        _hasMore = page.hasMore;
        _loading = false;
      });
    } catch (error) {
      if (!mounted) return;
      setState(() {
        _error = error;
        _loading = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    if (_items.isEmpty && _loading) {
      return G13Scaffold(
        title: '账单',
        onBack: () => Navigator.of(context).maybePop(),
        body: const Center(child: CircularProgressIndicator()),
      );
    }
    if (_items.isEmpty && _error != null) {
      return G13Scaffold(
        title: '账单',
        onBack: () => Navigator.of(context).maybePop(),
        body: FailureView(error: _error!, onRetry: () => _load(reset: true)),
      );
    }
    return G13Scaffold(
      title: '账单',
      onBack: () => Navigator.of(context).maybePop(),
      body: Column(
        children: [
          if (_error != null)
            InlineFailure(error: _error!, onRetry: () => _load(reset: false)),
          Expanded(
            child: _items.isEmpty
                ? const EmptyView(text: '还没有账单记录')
                : ListView.separated(
                    itemCount: _items.length + (_hasMore ? 1 : 0),
                    separatorBuilder: (_, _) => const Divider(height: 1),
                    itemBuilder: (context, index) {
                      if (index == _items.length) {
                        return Padding(
                          padding: const EdgeInsets.all(12),
                          child: Center(
                            child: _loading
                                ? const CircularProgressIndicator()
                                : OutlinedButton(
                                    onPressed: () => _load(reset: false),
                                    child: const Text('加载更多'),
                                  ),
                          ),
                        );
                      }
                      final entry = _items[index];
                      final incoming = entry.direction == 'in';
                      return ListTile(
                        title: Text(entry.title),
                        subtitle: Text(
                          '${formatTime(entry.createdAt)} · ${entry.state}',
                        ),
                        trailing: MoneyText(
                          money: entry.amount,
                          format: widget.ports.format,
                          style: TextStyle(
                            color: incoming ? Colors.green : null,
                          ),
                        ),
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }
}

class BankCardsPage extends StatefulWidget {
  const BankCardsPage({super.key, required this.ports, this.tokenProvider});

  final G13Ports ports;

  /// 卡号由供应商 SDK 换成 token 后才允许进业务层；页面不收集卡号。
  /// 平台桥未接入时按钮给出明确提示，不做假的"添加成功"。
  final Future<String?> Function()? tokenProvider;

  @override
  State<BankCardsPage> createState() => _BankCardsPageState();
}

class _BankCardsPageState extends State<BankCardsPage> {
  late Future<List<BankCard>> _future = widget.ports.commerce.bankCards();
  String? _pendingId;

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.commerce.bankCards();
    });
  }

  void _report(Object error) {
    if (!mounted) return;
    final text = error is G13Failure ? error.displayText : '$error';
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  Future<void> _add() async {
    final provider = widget.tokenProvider;
    if (provider == null) {
      _report(
        const G13Failure(
          G13FailureKind.unavailable,
          'PAY_SDK_NOT_WIRED',
          '添加银行卡需要平台支付 SDK 提供卡 token',
        ),
      );
      return;
    }
    final token = await provider();
    if (token == null || token.isEmpty) return;
    try {
      await widget.ports.commerce.addBankCard(
        cardToken: token,
        idempotencyKey: 'bind-$token',
      );
      _reload();
    } catch (error) {
      _report(error);
    }
  }

  Future<void> _remove(BankCard card) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('解绑这张卡？'),
        content: Text('${card.bank} ${card.maskedNumber} 解绑后需要重新验证才能添加。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(context).pop(false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(context).pop(true),
            child: const Text('解绑'),
          ),
        ],
      ),
    );
    if (confirmed != true) return;
    setState(() => _pendingId = card.id);
    try {
      await widget.ports.commerce.removeBankCard(
        card.id,
        idempotencyKey: 'unbind-${card.id}',
      );
      _reload();
    } catch (error) {
      _report(error);
    } finally {
      if (mounted) setState(() => _pendingId = null);
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '银行卡',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _add,
        icon: const Icon(Icons.add),
        label: const Text('添加'),
      ),
      body: AsyncView<List<BankCard>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (cards) => cards.isEmpty,
        emptyText: '还没有绑定银行卡',
        emptyHint: '添加后可用于充值、提现与支付',
        builder: (context, cards) => ListView.separated(
          itemCount: cards.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (context, index) {
            final card = cards[index];
            return ListTile(
              leading: const Icon(Icons.credit_card),
              title: Text('${card.bank} ${card.maskedNumber}'),
              subtitle: Text(card.isDefault ? '默认支付方式' : '已绑定'),
              trailing: _pendingId == card.id
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : PopupMenuButton<String>(
                      onSelected: (value) {
                        if (value == 'remove') _remove(card);
                      },
                      itemBuilder: (context) => const [
                        PopupMenuItem(value: 'remove', child: Text('解绑')),
                      ],
                    ),
            );
          },
        ),
      ),
    );
  }
}
