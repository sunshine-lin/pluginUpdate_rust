; NSIS 安装钩子（DEV-125226）
;
; 目的：安装时自动放行 17653 端口的入站连接，让开发机上的 AI 能远程读取本机日志。
; 不放行的话，日志服务虽然绑了 0.0.0.0，Windows 防火墙仍会拦掉跨机器的连接
; ——表现为端口不通、扫描发现不了这台机器。
;
; # 为什么是「尽力而为」而不是硬性保证
; 本安装包用 installMode: currentUser（见 tauri.conf.json）——那是 d8e50f5 有意
; 选的，改 perMachine 会让自动更新重新因提权失败而中断。而 netsh advfirewall
; **必须管理员权限**，与不提权的安装模式天然冲突。
;
; 故这里只在恰好有管理员权限时才执行（比如手工右键「以管理员身份运行」安装）。
; 没有权限就静默跳过，安装照常完成——绝不能因为防火墙规则加不上就让安装失败，
; 那会把「远程读日志」这个附加能力变成「客户端装不上」的致命问题。
;
; 权限不足时的补救：在目标机器上以管理员身份跑一次
;   netsh advfirewall firewall add rule name="aichat-updater" dir=in action=allow protocol=TCP localport=17653
; 规则绑端口而非程序路径，故客户端后续升级不需要重新执行。

!macro NSIS_HOOK_POSTINSTALL
  UserInfo::GetAccountType
  Pop $0
  ${If} $0 == "Admin"
    ; 先删同名规则再加，避免重复安装堆出多条一样的规则
    nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="aichat-updater"'
    Pop $0
    nsExec::ExecToLog 'netsh advfirewall firewall add rule name="aichat-updater" dir=in action=allow protocol=TCP localport=17653'
    Pop $0
    DetailPrint "已尝试放行 17653 端口（返回码 $0）"
  ${Else}
    DetailPrint "非管理员安装，跳过防火墙放行；如需 AI 远程读日志，请以管理员身份执行："
    DetailPrint 'netsh advfirewall firewall add rule name="aichat-updater" dir=in action=allow protocol=TCP localport=17653'
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; 卸载时清掉规则，不留孤立的防火墙配置
  UserInfo::GetAccountType
  Pop $0
  ${If} $0 == "Admin"
    nsExec::ExecToLog 'netsh advfirewall firewall delete rule name="aichat-updater"'
    Pop $0
  ${EndIf}
!macroend
