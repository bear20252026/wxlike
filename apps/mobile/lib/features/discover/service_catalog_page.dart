/// G13-04：服务目录 → 报价 → 订单。
///
/// 不复用"外链首页"充数：每个服务都从 `service.catalog` 读取，报价必须显示
/// 金额、币种和退改条款，下单返回作业句柄，履约状态以服务端为准。
library;

import 'package:flutter/material.dart';

import '../content/g13/core.dart';
import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class ServiceCatalogPage extends StatefulWidget {
  const ServiceCatalogPage({
    super.key,
    required this.ports,
    this.category = '',
  });

  final G13Ports ports;
  final String category;

  @override
  State<ServiceCatalogPage> createState() => _ServiceCatalogPageState();
}

class _ServiceCatalogPageState extends State<ServiceCatalogPage> {
  late Future<PageResult<ServiceItem>> _future = widget.ports.discovery
      .services(category: widget.category);

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.discovery.services(category: widget.category);
    });
  }

  Future<void> _openQuote(ServiceItem item) async {
    try {
      final quote = await widget.ports.discovery.quote(item.id);
      if (!mounted) return;
      await showModalBottomSheet<void>(
        context: context,
        showDragHandle: true,
        builder: (sheetContext) =>
            _QuoteSheet(ports: widget.ports, item: item, quote: quote),
      );
    } catch (error) {
      if (!mounted) return;
      final text = error is G13Failure ? error.displayText : '$error';
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: widget.category == 'shopping'
          ? '购物'
          : widget.category == 'game'
          ? '游戏'
          : '服务',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<PageResult<ServiceItem>>(
        future: _future,
        onRetry: _reload,
        isEmpty: (page) => page.isEmpty,
        emptyText: '这个分类还没有服务',
        builder: (context, page) => ListView.separated(
          itemCount: page.items.length,
          separatorBuilder: (_, _) => const Divider(height: 1),
          itemBuilder: (context, index) {
            final item = page.items[index];
            return ListTile(
              leading: const Icon(Icons.storefront_outlined),
              title: Text(item.title),
              subtitle: Text(item.provider),
              trailing: item.fromPrice == null
                  ? const Text('询价')
                  : MoneyText(
                      money: item.fromPrice,
                      format: widget.ports.format,
                    ),
              onTap: () => _openQuote(item),
            );
          },
        ),
      ),
    );
  }
}

class _QuoteSheet extends StatefulWidget {
  const _QuoteSheet({
    required this.ports,
    required this.item,
    required this.quote,
  });

  final G13Ports ports;
  final ServiceItem item;
  final ServiceQuote quote;

  @override
  State<_QuoteSheet> createState() => _QuoteSheetState();
}

class _QuoteSheetState extends State<_QuoteSheet> {
  OperationHandle? _order;
  Object? _error;
  bool _busy = false;

  Future<void> _createOrder() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final order = await widget.ports.discovery.createOrder(
        widget.quote.quoteId,
        idempotencyKey: 'order-${widget.quote.quoteId}',
      );
      if (mounted) setState(() => _order = order);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _refreshOrder() async {
    final order = _order;
    if (order == null) return;
    setState(() => _busy = true);
    try {
      final next = await widget.ports.discovery.order(order.operationId);
      if (mounted) setState(() => _order = next);
    } catch (error) {
      if (mounted) setState(() => _error = error);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final order = _order;
    return SafeArea(
      child: Padding(
        padding: const EdgeInsets.all(20),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(
              widget.item.title,
              style: Theme.of(context).textTheme.titleMedium,
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                const Text('报价 '),
                MoneyText(
                  money: widget.quote.price,
                  format: widget.ports.format,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
                const SizedBox(width: 8),
                Text(
                  '· ${formatTime(widget.quote.expiresAt)} 前有效',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ],
            ),
            const SizedBox(height: 8),
            Text(widget.quote.terms),
            if (_error != null) ...[
              const SizedBox(height: 8),
              InlineFailure(error: _error!),
            ],
            if (order != null) ...[
              const SizedBox(height: 12),
              Text('订单状态：${order.state}'),
              LinearProgressIndicator(value: order.progress.clamp(0, 1)),
              const SizedBox(height: 8),
              Align(
                alignment: Alignment.centerRight,
                child: TextButton(
                  onPressed: _busy ? null : _refreshOrder,
                  child: const Text('刷新'),
                ),
              ),
            ],
            const SizedBox(height: 12),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton(
                onPressed: _busy ? null : (order == null ? _createOrder : null),
                child: Text(order == null ? '创建订单' : '已创建'),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
