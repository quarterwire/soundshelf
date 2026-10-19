//! Native OLE drag-out of files, so the app behaves like Explorer as a drag source.
//! Premiere, DaVinci, Audition, etc. all accept the shell data object this produces.

use std::path::PathBuf;

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
    use windows::Win32::System::Com::{CoTaskMemFree, IDataObject};
    use windows::Win32::System::Ole::{
        DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DoDragDrop, IDropSource, IDropSource_Impl, OleFlushClipboard,
        OleInitialize, OleSetClipboard,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHParseDisplayName};
    use windows::core::{BOOL, HRESULT, PCWSTR, implement};

    #[implement(IDropSource)]
    struct DropSource;

    impl IDropSource_Impl for DropSource_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            if escape.as_bool() {
                DRAGDROP_S_CANCEL
            } else if (keys & MK_LBUTTON).0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    struct Pidls(Vec<*mut ITEMIDLIST>);

    impl Drop for Pidls {
        fn drop(&mut self) {
            for &p in &self.0 {
                unsafe { CoTaskMemFree(Some(p as *const _)) };
            }
        }
    }

    fn data_object(paths: &[PathBuf]) -> windows::core::Result<IDataObject> {
        unsafe {
            let _ = OleInitialize(None);
            let mut pidls = Pidls(Vec::with_capacity(paths.len()));
            for path in paths {
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let mut pidl = std::ptr::null_mut();
                SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)?;
                pidls.0.push(pidl);
            }
            let list: Vec<*const ITEMIDLIST> = pidls.0.iter().map(|&p| p as *const _).collect();
            // Same data object Explorer hands out (CF_HDROP, Shell IDList, …); items may span folders.
            let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&list)?;
            items.BindToHandler(None, &BHID_DataObject)
        }
    }

    pub fn drag_files(paths: &[PathBuf]) -> Result<(), String> {
        let data = data_object(paths).map_err(|e| e.to_string())?;
        let source: IDropSource = DropSource.into();
        let mut effect = DROPEFFECT::default();
        let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_LINK, &mut effect) };
        if hr.is_err() {
            return Err(hr.message());
        }
        Ok(())
    }

    pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
        let data = data_object(paths).map_err(|e| e.to_string())?;
        unsafe {
            OleSetClipboard(&data).map_err(|e| e.to_string())?;
            OleFlushClipboard().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    pub fn drag_files(_paths: &[PathBuf]) -> Result<(), String> {
        Err("drag-out is only implemented on Windows".into())
    }

    pub fn copy_files(_paths: &[PathBuf]) -> Result<(), String> {
        Err("file copy is only implemented on Windows".into())
    }
}

/// Starts a blocking OS drag of the files. Must be called while the left mouse button is held.
pub fn drag_files(paths: &[PathBuf]) -> Result<(), String> {
    imp::drag_files(paths)
}

/// Puts the files on the clipboard the same way Explorer's Ctrl+C does.
pub fn copy_files(paths: &[PathBuf]) -> Result<(), String> {
    imp::copy_files(paths)
}
