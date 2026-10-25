/// G13-05：账号与安全。
///
/// 本页只呈现账号服务返回的安全状态；修改密码、设备下线、声音锁属于账号服务
/// （G02）的写操作，绑定未接入时不会摆出无事件的按钮。
library;

import 'package:flutter/material.dart';

import '../content/g13/models.dart';
import '../content/g13/ports.dart';
import '../content/g13/widgets.dart';

class AccountSecurityPage extends StatefulWidget {
  const AccountSecurityPage({super.key, required this.ports});

  final G13Ports ports;

  @override
  State<AccountSecurityPage> createState() => _AccountSecurityPageState();
}

class _AccountSecurityPageState extends State<AccountSecurityPage> {
  late Future<SecurityInfo> _future = widget.ports.profile.security();

  void _reload() {
    if (!mounted) return;
    setState(() {
      _future = widget.ports.profile.security();
    });
  }

  @override
  Widget build(BuildContext context) {
    return G13Scaffold(
      title: '账号与安全',
      onBack: () => Navigator.of(context).maybePop(),
      actions: [
        IconButton(onPressed: _reload, icon: const Icon(Icons.refresh)),
      ],
      body: AsyncView<SecurityInfo>(
        future: _future,
        onRetry: _reload,
        builder: (context, info) => ListView(
          children: [
            SettingsTile(
              icon: Icons.phone_iphone,
              title: '手机号',
              subtitle: info.phoneMasked.isEmpty ? '未绑定' : info.phoneMasked,
            ),
            SettingsTile(
              icon: Icons.password,
              title: '登录密码',
              subtitle: info.passwordSet ? '已设置' : '未设置',
              trailing: Pill(
                text: info.passwordSet ? '已设置' : '未设置',
                color: info.passwordSet
                    ? const Color(0xff07c160)
                    : Colors.orange,
              ),
            ),
            SettingsTile(
              icon: Icons.hearing,
              title: '声音锁',
              subtitle: info.soundLock ? '已开启' : '未开启',
            ),
            SettingsTile(
              icon: Icons.devices_outlined,
              title: '已登录设备',
              subtitle: '${info.deviceCount} 台',
            ),
            SettingsTile(
              icon: Icons.verified_user_outlined,
              title: '上次安全验证',
              subtitle: formatTime(info.lastVerifiedAt),
            ),
            const Padding(
              padding: EdgeInsets.all(16),
              child: Text(
                '修改密码、设备下线与声音锁设置由账号服务（G02）提供；'
                'G13 只展示服务端状态，不在本地模拟修改结果。',
                style: TextStyle(fontSize: 12),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
