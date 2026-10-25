/// 四个供应商适配器共享的内部导入面：导入头收成一行，类型与工具只在
/// 这里列一次，适配器各自专注业务逻辑。
pub(crate) mod prelude {
    pub use crate::catalog::ServiceType;
    pub use crate::clock::Clock;
    pub use crate::error::{Result, ServiceError, ServiceErrorCode as Code};
    pub use crate::ids::new_id;
    pub use crate::provider::{
        Availability, ProviderCancellation, ProviderError, ProviderOrder, ProviderOrderRequest,
        ProviderOrderState, ProviderReceipt, ProviderResult, ProviderStatus, QuoteContext,
        SandboxEvidence, ServiceProvider,
    };
}

pub mod games;
pub mod reservations;
pub mod retail;
pub mod utilities;

pub use games::{ExternalLink, GamePackage, GameService, GameSignatureVerifier, LaunchTicket};
pub use reservations::{
    AuditEntry, Booking, BookingStatus, EncryptedProfile, Hold, Offer, ReservationKind,
    ReservationService, ReservationVendor, SensitiveProfile, SensitiveStore,
};
pub use retail::{
    Cart, CartItem, Product, RetailService, RetailVendor, VendorOrder, VendorOrderStatus,
};
pub use utilities::{
    sign_bill, DonationProject, ReceiptState, UtilitiesProvider, UtilitiesVendor, VendorBill,
    VendorDonation, VendorReceipt,
};
