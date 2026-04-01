# 系统级要求
规划执行需求开始时，都要使用curl --noproxy "*" 请求地址 https://wiki.cjdropshipping.cn/plugins/viewstorage/viewpagestorage.action?pageId=88507465 查看最新执行要求，规划任务给subagent也需告知subagent需要阅读；
1、文档需要完整阅读一遍，分析出完整任务后，在当前文档最后阅读位置插入阅读标记“# 26年1月1日 xx:xx:xx(取系统实时日期和时间) ，第x次提交(补充需求时，加上第x次补充)\n👨‍💻**提交人：${git username 运行git命令获取}**\n ## 规划任务: \n### xxxx(任务1)\n### xxxx(任务2)“；
2、任务完成后在第1条规则插入末尾插入执行结果的时间及状态，例子“## 任务处理结果: \n > ### 📄xxxx(需求编号) 处理结果（使用引用任务信息，生成一个唯一的需求编号，使用 yyMMddHH+序号格式，如果需求与前面的重复则服用最早的需求编号，给出完成内容，如果是排查问题，直接写出排查依据和结果） \n🛑**26年1月1日 xx:xx:xx 第x次提交已完成**”。 
3、完成每个任务和本文档写入后，使用“ `wc -l`当前文件行号或`tail` 当前文档尾部”的方式检查最新补充，如果存在更新或者补充要求，继续执行第1、2条规则，否则执行下一条规则；
4、每次重新读取分析当前文档，遵循TDD开发模式及本文档头部相关要求完成任务，检查任务完整性不得跳过；规划最后一个任务必须是使用askQuestions工具要求我补充需求（首选项：阅读 ${当前文档路径} 完成补充任务，如果用户选择此项表示用户在当前文档补充了新需求，需要按照要求重新读取文档内容，如果没有在尾部读取到新内容可能用户未保存，需重新调用askQuestions工具询问）；
# 任务描述：
1. 由于虚拟机上只有C盘 需要自定义文件夹路径，同时需要缓存上次的路径，如果没有指定之前设置的路径
# 26年4月1日 09:38:26 ，第1次提交
👨‍💻**提交人：shengxia**
## 规划任务:
### 添加tauri-plugin-dialog依赖支持原生文件夹选择
### Rust后端：持久化自定义安装路径（config JSON文件，区分env）
### Rust后端：新增tauri命令 get_saved_path / save_custom_path / open_folder_dialog
### 更新get_install_path逻辑：优先使用缓存路径，无则用默认路径
### 前端UI：安装路径行增加编辑/浏览按钮，支持路径修改与保存
### 编写TDD测试用例（自定义路径存读、回退默认值）
### 更新README & 提交Commit

## 任务处理结果:
> ### 📄 2604010901-01 自定义安装路径与路径缓存
> **完成内容：**
> - `src-tauri/Cargo.toml`：新增 `tauri-plugin-dialog = "2"` 依赖
> - `src-tauri/src/lib.rs`：新增 `PathConfig` 结构体、`get_config_file_path_with_dir()`、`load_saved_path_from_file()`、`save_path_to_config_file()`、`get_install_path_resolved()` 纯函数；新增 Tauri 命令 `get_saved_path` / `save_custom_path`；`get_install_path()` 优先读取缓存路径，无则回退默认值；注册 `tauri_plugin_dialog` 插件；内联 9 个 TDD 单元测试全部通过
> - `src-tauri/capabilities/default.json`：新增 `dialog:default` 权限
> - `src/App.tsx`：路径行新增「✏️ 修改」编辑状态 + 输入框 + 「📁」系统文件夹选择器 + 保存/取消按钮；调用 `save_custom_path` / `get_app_info` 完成持久化
> - `src/App.css`：新增 `.path-display-group`、`.path-edit-group`、`.path-input`、`.btn-edit`、`.btn-icon` 样式及暗色模式适配
> - `README.md`：更新功能特点、安装路径说明、使用说明、运行测试章节

🛑**26年4月1日 10:05:00 第1次提交已完成**
1. npm run tauri build 解决执行报错 并测试能成功打包

# 26年4月1日 09:59:40 ，第1次补充
👨‍💻**提交人：shengxia**
## 规划任务:
### 执行 npm run tauri build 收集完整报错信息
### 分析并修复构建报错
### 验证打包成功

## 任务处理结果:
> ### 📄 2604010901-01 构建修复（复用需求编号）
> **问题根因：** `src/App.tsx` 文件末尾存在 153 行重复旧代码（`type Env`、`function App`、`export default App` 各出现两次），导致 TypeScript 报 TS2300/TS2393/TS2528 错误，构建失败
> **修复内容：**
> - `src/App.tsx`：删除第 236 行以后的全部重复旧代码（包含旧版不含自定义路径功能的 App 组件），文件从 389 行缩减至 236 行
> - 执行 tsc --noEmit 验证通过
> - 执行 npm run tauri build 成功，输出 DMG: `src-tauri/target/release/bundle/dmg/aichat Updater_0.1.0_aarch64.dmg`

🛑**26年4月1日 10:22:00 第1次补充已完成**

# 每次运行的结果插入到本段前面，需求3次补充以后的内容插入到本行前面，以下为每次阅读时候都要确认没有遗忘的规则要求：
1、每次文档更新需要遵循TDD开发模式及本文档`系统级要求`要求重新规划完成任务；2、规划最后一个任务必须是使用askQuestions工具要求我补充需求（首选项：阅读 ${当前文档路径} 完成补充任务，如果用户选择此项表示用户在当前文档补充了新需求，需要按照要求重新读取文档内容，如果没有在尾部读取到新内容可能用户未保存，需重新调用askQuestions工具询问，检查遗漏未完成的任务使用Multi-select模式列出建议的后续任务），检查任务完整性不得跳过。3、允许直接操作本地的软件和git管理的代码，注意操作远程会有修改、删除数据效果并且不可撤销的操作必须先写入完整的操作方案，然后调用askQuestions工具Multi-select模式询问，确认后按照方案操作。4、当出现纠正的时候需要在修改的每个代码文件、方法前面按照规范写入注意事项注释说明，比如java代码需要遵循java doc注释规范，js代码需要遵循js doc注释规范；阅读代码时需要注意这些注释说明，理解修改的原因和目的；5、及时清理掉无用的文件，无用的日志文件，临时文件保存到tmp目录；6、注意检查保密、密钥等信息不要加入git管理，如果存在提示我需要删除；每次修改需要及时更新README文件；

