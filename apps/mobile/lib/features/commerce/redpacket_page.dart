/// G13-03：红包。
///
/// 领取并发由服务端用行锁/串行化保证（MONEY-01）；界面只在拿到服务端结果后
/// 更新剩余额与"我已领取"，不本地先减库存。金额精确到分，展示走 Rust
/// 格式化，不出现 66.66 这类演示值。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class RedPacketOpenPage extends StatefulWidget {
  const RedPacketOpenPage({
    super.key,
    required this.ports,
    required this.packetId,
  });

  final G13Ports ports;
  final String packetId;

  @override
  State<RedPacketOpenPage> createState() => _RedPacketOpenPageState();
}

class _RedPacketOpenPageState extends State<RedPacketOpenPage>
    with SubmitGuard {
  late Future<RedPacket> _future = widget.ports.commerce.redPacket(
    widget.packetId,
  );
  RedPacket? _latest;
  String? _error;

  void _reload() {
    if (!mounted) return;
    setState(() {
      _error = null;
      _future = widget.ports.commerce.redPacket(widget.packetId);
    });
  }

  Future<void> _claim() async {
    await guard(() async {
      try {
        final packet = await widget.ports.commerce.claimRedPacket(
          widget.packetId,
          idempotencyKey: 'claim-${widget.packetId}',
        );
        if (!mounted) return;
        setState(() => _latest = packet);
      } catch (error) {
        if (!mounted) return;
        setState(
          () => _error = error is G13Failure ? error.displayText : '$error',
        );
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '红包',
      onBack: () => Navigator.of(context).maybePop(),
      body: AsyncView<RedPacket>(
        future: _future,
        onRetry: _reload,
        builder: (context, loaded) {
          final packet = _latest ?? loaded;
          return ListView(
            padding: const EdgeInsets.all(20),
            children: [
              Container(
                width: double.infinity,
                padding: const EdgeInsets.all(20),
                decoration: BoxDecoration(
                  color: const Color(0xffd8472f),
                  borderRadius: BorderRadius.circular(12),
                ),
                child: Column(
                  children: [
                    const Icon(
                      Icons.card_giftcard,
                      color: Colors.white,
                      size: 36,
                    ),
                    const SizedBox(height: 8),
                    if (packet.claimed && packet.claimedAmount != null)
                      MoneyText(
                        money: packet.claimedAmount,
                        format: widget.ports.format,
                        style: const TextStyle(
                          color: Colors.white,
                          fontSize: 32,
                        ),
                      )
                    else
                      Text(
                        packet.exhausted ? '红包已领完' : '共 ${packet.count} 个红包',
                        style: const TextStyle(
                          color: Colors.white,
                          fontSize: 18,
                        ),
                      ),
                    const SizedBox(height: 8),
                    Text(
                      '已领取 ${packet.count - packet.remainingCount}/${packet.count}',
                      style: const TextStyle(color: Colors.white70),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: 16),
              ListTile(
                title: const Text('总额'),
                trailing: MoneyText(
                  money: packet.total,
                  format: widget.ports.format,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
              ListTile(
                title: const Text('剩余'),
                trailing: MoneyText(
                  money: packet.remaining,
                  format: widget.ports.format,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
              ListTile(
                title: const Text('过期时间'),
                trailing: Text(formatTime(packet.expiresAt)),
              ),
              if (_error != null) ...[
                const SizedBox(height: 8),
                InlineFailure(
                  error: G13Failure(
                    G13FailureKind.unexpected,
                    'REDPACKET',
                    _error!,
                  ),
                ),
              ],
              const SizedBox(height: 16),
              FilledButton(
                onPressed: packet.claimed || packet.exhausted || submitting
                    ? null
                    : _claim,
                child: submitting
                    ? const SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : Text(
                        packet.claimed
                            ? '已领取'
                            : packet.exhausted
                            ? '已领完'
                            : '开红包',
                      ),
              ),
            ],
          );
        },
      ),
    );
  }
}

class RedPacketCreatePage extends StatefulWidget {
  const RedPacketCreatePage({
    super.key,
    required this.ports,
    required this.convId,
  });

  final G13Ports ports;
  final String convId;

  @override
  State<RedPacketCreatePage> createState() => _RedPacketCreatePageState();
}

class _RedPacketCreatePageState extends State<RedPacketCreatePage>
    with SubmitGuard {
  final TextEditingController _amount = TextEditingController();
  final TextEditingController _count = TextEditingController(text: '1');
  final TextEditingController _greeting = TextEditingController(
    text: '恭喜发财，大吉大利',
  );
  String? _error;

  @override
  void dispose() {
    _amount.dispose();
    _count.dispose();
    _greeting.dispose();
    super.dispose();
  }

  Future<void> _create() async {
    final minor = parseMinorAmount(_amount.text.trim());
    final count = int.tryParse(_count.text.trim());
    if (minor == null || minor <= 0 || count == null || count <= 0) {
      setState(() => _error = '金额或个数不合法');
      return;
    }
    await guard(() async {
      try {
        final packet = await widget.ports.commerce.createRedPacket(
          convId: widget.convId,
          total: Money(minor: minor, currency: 'CNY'),
          count: count,
          greeting: _greeting.text.trim(),
          idempotencyKey:
              'rp-create-${widget.convId}-$minor-$count-${_greeting.text.trim().hashCode}',
        );
        if (!mounted) return;
        await Navigator.of(context).pushReplacement(
          MaterialPageRoute<void>(
            builder: (_) =>
                RedPacketOpenPage(ports: widget.ports, packetId: packet.id),
          ),
        );
      } catch (error) {
        if (!mounted) return;
        setState(
          () => _error = error is G13Failure ? error.displayText : '$error',
        );
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '发红包',
      onBack: () => Navigator.of(context).maybePop(),
      body: ListView(
        padding: const EdgeInsets.all(20),
        children: [
          TextField(
            controller: _amount,
            keyboardType: const TextInputType.numberWithOptions(decimal: true),
            decoration: const InputDecoration(
              labelText: '总金额（元）',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _count,
            keyboardType: TextInputType.number,
            decoration: const InputDecoration(
              labelText: '红包个数',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: 12),
          TextField(
            controller: _greeting,
            maxLength: 30,
            decoration: const InputDecoration(
              labelText: '祝福语',
              border: OutlineInputBorder(),
            ),
          ),
          if (_error != null)
            InlineFailure(
              error: G13Failure(
                G13FailureKind.invalidArgument,
                'RP_INPUT',
                _error!,
              ),
            ),
          const SizedBox(height: 12),
          FilledButton(
            onPressed: submitting ? null : _create,
            child: submitting
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Text('塞钱进红包'),
          ),
        ],
      ),
    );
  }
}
